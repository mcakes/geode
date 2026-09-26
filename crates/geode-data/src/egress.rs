//! Uploads to egress targets. The service thread resolves the target, the
//! address and the document kind, writes the document to bytes, and hands
//! them to that target's worker thread, so a slow transport never blocks the
//! request loop. One worker per target runs its uploads one at a time, in
//! submission order, behind a queue of [`EGRESS_QUEUE_BOUND`] waiting jobs.
//!
//! A serviced upload normally emits one `DataEvent::Upload`. Validation,
//! serialization, and queue refusals answer on the service thread; transport
//! results answer from the worker. Transport panics become errors naming the
//! target, and the worker continues with queued jobs. Serialization has no panic
//! boundary, neither call has a timeout, and the event sink can refuse an outcome.
//! There is no automatic retry.
//!
//! Shutdown drops the job senders and joins the workers. A worker finishes
//! the jobs already queued first, so shutdown can wait on a slow transport;
//! run it off the UI thread, as every other `DataService` shutdown.

use crate::adapter::{AdapterRegistry, Egress};
use crate::documents::DocumentRegistry;
use crate::service::{DataEvent, EventSink};
use geode_core::config::{Diagnostic, Severity};
use geode_core::document::DocumentRows;
use geode_core::egress_config::EgressSpec;
use geode_core::query::QueryKey;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread::JoinHandle;

/// Uploads waiting behind the one in flight, per target. Past this the
/// upload answers `queue full` at once rather than waiting.
pub const EGRESS_QUEUE_BOUND: usize = 8;

/// One upload, as a tile asks for it.
#[derive(Debug)]
pub struct UploadParams {
    /// The requesting tile.
    pub key: QueryKey,
    /// The tile's upload counter, echoed in the outcome.
    pub tag: u64,
    pub target: String,
    /// Document dataset name; also the document kind's name.
    pub document: String,
    pub rows: DocumentRows,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UploadOutcome {
    pub key: QueryKey,
    pub tag: u64,
    pub target: String,
    pub result: Result<(), String>,
}

/// Drop targets whose adapter is unknown or has no egress side, with a
/// diagnostic each (path `egress.<name>.adapter`). Called by geode-app at
/// startup; the survivors go into `DataServiceConfig::egress`.
pub fn resolve(
    specs: Vec<EgressSpec>,
    adapters: &AdapterRegistry,
) -> (Vec<EgressSpec>, Vec<Diagnostic>) {
    let mut kept = Vec::new();
    let mut diags = Vec::new();
    for spec in specs {
        let problem = match adapters.get(&spec.adapter) {
            None => Some(format!("adapter '{}' is not in this build", spec.adapter)),
            Some(adapter) if adapter.egress().is_none() => {
                Some(format!("adapter '{}' has no egress side", spec.adapter))
            }
            Some(_) => None,
        };
        match problem {
            None => kept.push(spec),
            Some(message) => diags.push(Diagnostic {
                severity: Severity::Error,
                layer: None,
                file: None,
                message: format!("egress '{}': {message}", spec.name),
                path: Some(format!("egress.{}.adapter", spec.name)),
            }),
        }
    }
    (kept, diags)
}

/// A written upload waiting for its target's worker.
struct Job {
    key: QueryKey,
    tag: u64,
    document: String,
    document_key: String,
    address: String,
    bytes: Vec<u8>,
}

struct Target {
    spec: EgressSpec,
    /// `None` once stopped, or when no worker ever started (`unavailable`
    /// then says why).
    tx: Mutex<Option<SyncSender<Job>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
    unavailable: Option<String>,
}

/// The per-target workers `DataService` owns.
pub(crate) struct EgressWorkers {
    targets: HashMap<String, Target>,
    sink: EventSink,
}

/// Log one result and offer its outcome to the event sink. Sink refusal is
/// ignored here; logging does not establish that the tile received the outcome.
fn answer(
    sink: &EventSink,
    target: &str,
    document: &str,
    document_key: &str,
    key: QueryKey,
    tag: u64,
    result: Result<(), String>,
) {
    match &result {
        Ok(()) => tracing::info!(
            target: "geode::ingest",
            "upload to '{target}': {document} '{document_key}' sent"
        ),
        Err(e) => tracing::info!(
            target: "geode::ingest",
            "upload to '{target}': {document} '{document_key}' failed: {e}"
        ),
    }
    let _ = sink(DataEvent::Upload(UploadOutcome {
        key,
        tag,
        target: target.to_string(),
        result,
    }));
}

fn work(name: String, mut egress: Box<dyn Egress>, jobs: Receiver<Job>, sink: EventSink) {
    while let Ok(job) = jobs.recv() {
        // Convert transport panics into this upload's error and keep servicing the
        // queue. Mark the catch boundary so the app logs a contained panic without
        // creating a crash report.
        let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            geode_core::panic::contained(|| egress.upload(&job.address, job.bytes))
        })) {
            Ok(outcome) => outcome.map_err(|e| format!("egress '{name}': {e}")),
            Err(payload) => Err(format!(
                "egress '{name}': transport panicked: {}",
                crate::ingest::runner::panic_payload_message(&*payload)
            )),
        };
        answer(
            &sink,
            &name,
            &job.document,
            &job.document_key,
            job.key,
            job.tag,
            result,
        );
    }
}

impl EgressWorkers {
    /// Start one transport worker per target. Missing adapters, unavailable
    /// egress handles, and thread-start failures retain the target's name and
    /// reason so subsequent uploads can report the startup failure. The earlier
    /// `resolve` check obtains a separate handle and cannot guarantee this succeeds.
    pub(crate) fn spawn(specs: &[EgressSpec], adapters: &AdapterRegistry, sink: EventSink) -> Self {
        let mut targets = HashMap::new();
        for spec in specs {
            let egress = adapters.get(&spec.adapter).and_then(|a| a.egress());
            let mut target = Target {
                spec: spec.clone(),
                tx: Mutex::new(None),
                thread: Mutex::new(None),
                unavailable: None,
            };
            match egress {
                None => {
                    target.unavailable =
                        Some(format!("adapter '{}' has no egress side", spec.adapter));
                }
                Some(egress) => {
                    let (tx, rx) = sync_channel::<Job>(EGRESS_QUEUE_BOUND);
                    let name = spec.name.clone();
                    let worker_sink = EventSink::clone(&sink);
                    let spawned = std::thread::Builder::new()
                        .name(format!("geode-egress-{}", spec.name))
                        .spawn(move || work(name, egress, rx, worker_sink));
                    match spawned {
                        Ok(handle) => {
                            target.tx = Mutex::new(Some(tx));
                            target.thread = Mutex::new(Some(handle));
                        }
                        Err(e) => {
                            target.unavailable = Some(format!("could not start its worker: {e}"));
                        }
                    }
                }
            }
            if let Some(why) = &target.unavailable {
                tracing::warn!(target: "geode::ingest", "egress '{}': {why}", spec.name);
            }
            targets.insert(spec.name.clone(), target);
        }
        EgressWorkers { targets, sink }
    }

    /// Resolve, serialize, and queue an upload. Validation and queue refusals
    /// offer an error outcome on the service thread. The worker offers the
    /// transport result if it completes; panic and blocked-call limits are
    /// described in the module documentation.
    pub(crate) fn upload(&self, p: UploadParams, documents: &DocumentRegistry) {
        let document_key = p.rows.key.join("/");
        let refuse = |message: String| {
            answer(
                &self.sink,
                &p.target,
                &p.document,
                &document_key,
                p.key,
                p.tag,
                Err(format!("egress '{}': {message}", p.target)),
            );
        };
        let Some(target) = self.targets.get(&p.target) else {
            return refuse("unknown target".into());
        };
        let Some(address) = target.spec.address(&p.document, &p.rows.key) else {
            return refuse(format!("does not accept {}", p.document));
        };
        let Some(kind) = documents.get(&p.document) else {
            return refuse(format!("no document kind {}", p.document));
        };
        let bytes = match kind.write(&p.rows) {
            Ok(bytes) => bytes,
            Err(e) => return refuse(e.to_string()),
        };
        let guard = target.tx.lock().unwrap_or_else(|e| e.into_inner());
        let Some(tx) = guard.as_ref() else {
            drop(guard);
            return refuse(
                target
                    .unavailable
                    .clone()
                    .unwrap_or_else(|| "stopped".into()),
            );
        };
        let job = Job {
            key: p.key,
            tag: p.tag,
            document: p.document.clone(),
            document_key: document_key.clone(),
            address,
            bytes,
        };
        let refused = match tx.try_send(job) {
            Ok(()) => None,
            Err(TrySendError::Full(_)) => Some("queue full"),
            Err(TrySendError::Disconnected(_)) => Some("stopped"),
        };
        drop(guard);
        if let Some(why) = refused {
            refuse(why.into());
        }
    }

    /// Close every queue, then join every worker. Queued jobs still run and
    /// answer. Idempotent.
    pub(crate) fn shutdown(&self) {
        for target in self.targets.values() {
            drop(target.tx.lock().unwrap_or_else(|e| e.into_inner()).take());
        }
        for target in self.targets.values() {
            let handle = target
                .thread
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            if let Some(handle) = handle {
                let _ = handle.join();
            }
        }
    }
}

impl Drop for EgressWorkers {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::channel::ChannelAdapter;
    use crate::adapter::{Adapter, AdapterError, MESSAGE_BOUND, MessageSink, Subscription};
    use geode_core::document::{DocumentKind, ParseError, ParsedDocument, WriteError};
    use geode_core::schema::ColumnType;
    use std::sync::Arc;
    use std::sync::mpsc::{Receiver, channel};
    use std::time::Duration;

    const TARGET: &str = "sophis";
    const DIVIDEND: &str = "dividend_schedule";

    /// Writes the rows' key as its bytes; a key of `BAD` is a write error.
    struct KeyKind;

    impl DocumentKind for KeyKind {
        fn name(&self) -> &'static str {
            DIVIDEND
        }
        fn columns(&self) -> &[(&'static str, ColumnType)] {
            &[]
        }
        fn parse(&self, _bytes: &[u8]) -> Result<ParsedDocument, ParseError> {
            Err(ParseError {
                message: "KeyKind does not parse".into(),
            })
        }
        fn write(&self, rows: &DocumentRows) -> Result<Vec<u8>, WriteError> {
            if rows.key == ["BAD"] {
                return Err(WriteError {
                    message: "dividend 3 has no pay date".into(),
                });
            }
            Ok(rows.key.join("/").into_bytes())
        }
    }

    fn documents() -> DocumentRegistry {
        let mut documents = DocumentRegistry::default();
        documents.register(Arc::new(KeyKind));
        documents
    }

    fn spec(adapter: &str, documents: &[(&str, &str)]) -> EgressSpec {
        EgressSpec {
            name: TARGET.into(),
            adapter: adapter.into(),
            documents: documents
                .iter()
                .map(|(d, a)| (d.to_string(), a.to_string()))
                .collect(),
        }
    }

    fn dividend_spec() -> EgressSpec {
        spec(
            "demo_bus",
            &[
                (DIVIDEND, "marketdata/dividend/{key}"),
                ("orphan", "marketdata/orphan/{key}"),
            ],
        )
    }

    fn event_sink() -> (EventSink, Receiver<DataEvent>) {
        let (tx, rx) = channel();
        let tx = Mutex::new(tx);
        let sink: EventSink = Arc::new(move |e| tx.lock().unwrap().send(e).is_ok());
        (sink, rx)
    }

    fn params(tag: u64, target: &str, document: &str, key: &str) -> UploadParams {
        UploadParams {
            key: QueryKey(7),
            tag,
            target: target.into(),
            document: document.into(),
            rows: DocumentRows {
                key: vec![key.to_string()],
                attributes: Vec::new(),
                axes: Vec::new(),
                values: Vec::new(),
            },
        }
    }

    fn next_upload(rx: &Receiver<DataEvent>) -> UploadOutcome {
        match rx
            .recv_timeout(Duration::from_secs(5))
            .expect("an upload outcome")
        {
            DataEvent::Upload(outcome) => outcome,
            other => panic!("not an upload outcome: {other:?}"),
        }
    }

    fn assert_silent(rx: &Receiver<DataEvent>) {
        assert!(
            rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "each upload answers exactly once"
        );
    }

    fn channel_registry() -> (
        AdapterRegistry,
        Arc<ChannelAdapter>,
        crate::adapter::channel::ChannelFeed,
    ) {
        let (adapter, feed) = ChannelAdapter::new("demo_bus");
        let mut adapters = AdapterRegistry::default();
        adapters.register(adapter.clone());
        (adapters, adapter, feed)
    }

    #[test]
    fn an_upload_reaches_the_target_address_and_answers_ok() {
        let (adapters, adapter, _feed) = channel_registry();
        let (bus_sink, bus_rx) = MessageSink::bounded(8);
        let mut sub = adapter.subscription().unwrap();
        sub.subscribe(
            &["marketdata/dividend/>".into()],
            bus_sink,
            Arc::new(|_| {}),
        )
        .unwrap();
        let (sink, rx) = event_sink();
        let workers = EgressWorkers::spawn(&[dividend_spec()], &adapters, sink);

        workers.upload(params(3, TARGET, DIVIDEND, "XYZ"), &documents());

        let m = bus_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            (m.topic.as_str(), m.bytes.as_slice()),
            ("marketdata/dividend/XYZ", &b"XYZ"[..])
        );
        assert_eq!(
            next_upload(&rx),
            UploadOutcome {
                key: QueryKey(7),
                tag: 3,
                target: TARGET.into(),
                result: Ok(()),
            }
        );
        assert_silent(&rx);

        // A two-part document key joins with '/' in the address, same as
        // EgressSpec::address itself.
        let mut two_part = params(4, TARGET, DIVIDEND, "A");
        two_part.rows.key = vec!["A".to_string(), "B".to_string()];
        workers.upload(two_part, &documents());
        let m = bus_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(m.topic.as_str(), "marketdata/dividend/A/B");
        assert_eq!(next_upload(&rx).result, Ok(()));
        assert_silent(&rx);
    }

    #[test]
    fn a_write_error_an_unknown_target_and_a_closed_bus_each_answer_err_naming_the_target() {
        let (adapters, _adapter, feed) = channel_registry();
        let (sink, rx) = event_sink();
        let workers = EgressWorkers::spawn(&[dividend_spec()], &adapters, sink);
        let documents = documents();

        workers.upload(params(1, TARGET, DIVIDEND, "BAD"), &documents);
        let outcome = next_upload(&rx);
        assert_eq!((outcome.tag, outcome.target.as_str()), (1, TARGET));
        assert_eq!(
            outcome.result,
            Err("egress 'sophis': dividend 3 has no pay date".into())
        );
        assert_silent(&rx);

        workers.upload(params(2, "nowhere", DIVIDEND, "XYZ"), &documents);
        let outcome = next_upload(&rx);
        assert_eq!((outcome.tag, outcome.target.as_str()), (2, "nowhere"));
        assert_eq!(
            outcome.result,
            Err("egress 'nowhere': unknown target".into())
        );
        assert_silent(&rx);

        workers.upload(params(3, TARGET, "cvi_params", "XYZ"), &documents);
        assert_eq!(
            next_upload(&rx).result,
            Err("egress 'sophis': does not accept cvi_params".into())
        );
        assert_silent(&rx);

        workers.upload(params(4, TARGET, "orphan", "XYZ"), &documents);
        assert_eq!(
            next_upload(&rx).result,
            Err("egress 'sophis': no document kind orphan".into())
        );
        assert_silent(&rx);

        // No subscription, so no dispatcher drains the bus: it is full at
        // exactly its bound and the transport refuses the upload.
        for _ in 0..MESSAGE_BOUND {
            assert!(feed.publish("marketdata/other", Vec::new()));
        }
        workers.upload(params(5, TARGET, DIVIDEND, "XYZ"), &documents);
        let outcome = next_upload(&rx);
        assert_eq!(outcome.tag, 5);
        let err = outcome.result.unwrap_err();
        assert!(err.starts_with("egress 'sophis': "), "{err}");
        assert!(err.contains("the bus is full or closed"), "{err}");
        assert_silent(&rx);
    }

    #[test]
    fn uploads_to_one_target_run_in_submission_order() {
        let (adapters, adapter, _feed) = channel_registry();
        let (bus_sink, bus_rx) = MessageSink::bounded(32);
        let mut sub = adapter.subscription().unwrap();
        sub.subscribe(
            &["marketdata/dividend/>".into()],
            bus_sink,
            Arc::new(|_| {}),
        )
        .unwrap();
        let (sink, rx) = event_sink();
        let workers = EgressWorkers::spawn(&[dividend_spec()], &adapters, sink);
        let documents = documents();

        let keys = ["A", "B", "C", "D", "E", "F"];
        for (tag, key) in keys.iter().enumerate() {
            workers.upload(params(tag as u64, TARGET, DIVIDEND, key), &documents);
        }
        let tags: Vec<u64> = keys.iter().map(|_| next_upload(&rx).tag).collect();
        assert_eq!(tags, vec![0, 1, 2, 3, 4, 5]);
        let topics: Vec<String> = keys
            .iter()
            .map(|_| bus_rx.recv_timeout(Duration::from_secs(5)).unwrap().topic)
            .collect();
        let expected: Vec<String> = keys
            .iter()
            .map(|k| format!("marketdata/dividend/{k}"))
            .collect();
        assert_eq!(topics, expected);
    }

    /// An egress that announces each upload and then waits to be released,
    /// so a test can hold one upload in flight.
    struct GateEgress {
        entered: SyncSender<String>,
        release: Receiver<()>,
    }

    impl Egress for GateEgress {
        fn upload(&mut self, target: &str, _bytes: Vec<u8>) -> Result<(), AdapterError> {
            let _ = self.entered.send(target.to_string());
            let _ = self.release.recv();
            Ok(())
        }
    }

    struct GateAdapter {
        egress: Mutex<Option<Box<dyn Egress>>>,
    }

    impl Adapter for GateAdapter {
        fn name(&self) -> &'static str {
            "gate"
        }
        fn subscription(&self) -> Option<Box<dyn Subscription>> {
            None
        }
        fn egress(&self) -> Option<Box<dyn Egress>> {
            self.egress.lock().unwrap().take()
        }
    }

    #[test]
    fn a_full_queue_answers_err_at_once_and_the_queued_uploads_still_run() {
        let (entered_tx, entered) = sync_channel(64);
        let (release, release_rx) = sync_channel(64);
        let mut adapters = AdapterRegistry::default();
        adapters.register(Arc::new(GateAdapter {
            egress: Mutex::new(Some(Box::new(GateEgress {
                entered: entered_tx,
                release: release_rx,
            }))),
        }));
        let (sink, rx) = event_sink();
        let workers = EgressWorkers::spawn(
            &[spec("gate", &[(DIVIDEND, "gate/{key}")])],
            &adapters,
            sink,
        );
        // Rebound after `workers` so it drops first: a failing assert then
        // opens the gate before `workers`' drop joins the held worker,
        // instead of hanging on it.
        let release = release;
        let documents = documents();

        // One in flight, held at the gate...
        workers.upload(params(0, TARGET, DIVIDEND, "K0"), &documents);
        assert_eq!(
            entered.recv_timeout(Duration::from_secs(5)).unwrap(),
            "gate/K0"
        );
        // ...then the queue fills to its bound...
        for tag in 1..=EGRESS_QUEUE_BOUND as u64 {
            workers.upload(params(tag, TARGET, DIVIDEND, "K"), &documents);
        }
        assert_silent(&rx);
        // ...and the next is refused without waiting.
        let over = EGRESS_QUEUE_BOUND as u64 + 1;
        workers.upload(params(over, TARGET, DIVIDEND, "K"), &documents);
        let outcome = next_upload(&rx);
        assert_eq!(outcome.tag, over);
        assert_eq!(outcome.result, Err("egress 'sophis': queue full".into()));

        for _ in 0..=EGRESS_QUEUE_BOUND {
            release.send(()).unwrap();
        }
        let tags: Vec<u64> = (0..=EGRESS_QUEUE_BOUND)
            .map(|_| next_upload(&rx).tag)
            .collect();
        assert_eq!(tags, (0..=EGRESS_QUEUE_BOUND as u64).collect::<Vec<_>>());
        assert_silent(&rx);
    }

    #[test]
    fn after_shutdown_an_upload_answers_err_naming_the_target() {
        let (adapters, _adapter, _feed) = channel_registry();
        let (sink, rx) = event_sink();
        let workers = EgressWorkers::spawn(&[dividend_spec()], &adapters, sink);
        workers.shutdown();
        workers.shutdown();
        workers.upload(params(1, TARGET, DIVIDEND, "XYZ"), &documents());
        assert_eq!(
            next_upload(&rx).result,
            Err("egress 'sophis': stopped".into())
        );
        assert_silent(&rx);
    }

    /// The message a [`PanickingEgress`] transport panics with. A constant so
    /// the assertion and the panic that produced it cannot drift apart.
    const UPLOAD_PANIC: &str = "the transport fell over";

    /// Panic for the configured number of calls, then succeed. The fixture
    /// exercises both the error outcome and continued use of the same worker.
    struct PanickingEgress {
        panics_left: usize,
    }

    impl Egress for PanickingEgress {
        fn upload(&mut self, _target: &str, _bytes: Vec<u8>) -> Result<(), AdapterError> {
            if self.panics_left > 0 {
                self.panics_left -= 1;
                panic!("{UPLOAD_PANIC}");
            }
            Ok(())
        }
    }

    struct PanicAdapter {
        egress: Mutex<Option<Box<dyn Egress>>>,
    }

    impl Adapter for PanicAdapter {
        fn name(&self) -> &'static str {
            "panic"
        }
        fn subscription(&self) -> Option<Box<dyn Subscription>> {
            None
        }
        fn egress(&self) -> Option<Box<dyn Egress>> {
            self.egress.lock().unwrap().take()
        }
    }

    #[test]
    fn a_panicking_transport_answers_the_upload_and_keeps_the_worker() {
        let mut adapters = AdapterRegistry::default();
        adapters.register(Arc::new(PanicAdapter {
            egress: Mutex::new(Some(Box::new(PanickingEgress { panics_left: 1 }))),
        }));
        let (sink, rx) = event_sink();
        let workers = EgressWorkers::spawn(
            &[spec("panic", &[(DIVIDEND, "panic/{key}")])],
            &adapters,
            sink,
        );
        let documents = documents();

        workers.upload(params(1, TARGET, DIVIDEND, "K0"), &documents);
        let first = next_upload(&rx);
        let message = first.result.expect_err("a panicking transport is an error");
        assert!(
            message.contains(UPLOAD_PANIC),
            "the answer carries the panic payload: {message}"
        );
        assert!(
            message.contains(&format!("egress '{TARGET}'")),
            "the answer names its target: {message}"
        );
        assert_silent(&rx);

        workers.upload(params(2, TARGET, DIVIDEND, "K1"), &documents);
        let second = next_upload(&rx);
        assert_eq!(
            second.result,
            Ok(()),
            "the worker survived, so the next upload succeeds"
        );
        assert_silent(&rx);
    }

    #[test]
    fn resolve_drops_an_unknown_adapter_and_one_without_egress() {
        let (mut adapters, _adapter, _feed) = channel_registry();
        // A gate adapter whose one egress is already gone answers `None`.
        adapters.register(Arc::new(GateAdapter {
            egress: Mutex::new(None),
        }));
        let kept_spec = dividend_spec();
        let mut unknown = spec("carrier_pigeon", &[(DIVIDEND, "x")]);
        unknown.name = "pigeon".into();
        let mut closed = spec("gate", &[(DIVIDEND, "x")]);
        closed.name = "gated".into();

        let (kept, diags) = resolve(vec![unknown, kept_spec.clone(), closed], &adapters);

        assert_eq!(kept, vec![kept_spec]);
        let seen: Vec<(Option<&str>, &str)> = diags
            .iter()
            .map(|d| (d.path.as_deref(), d.message.as_str()))
            .collect();
        assert_eq!(
            seen,
            vec![
                (
                    Some("egress.pigeon.adapter"),
                    "egress 'pigeon': adapter 'carrier_pigeon' is not in this build"
                ),
                (
                    Some("egress.gated.adapter"),
                    "egress 'gated': adapter 'gate' has no egress side"
                ),
            ]
        );
        assert!(diags.iter().all(|d| d.severity == Severity::Error));
    }
}
