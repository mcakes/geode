//! Commands to the position system. `positions.toml` names one service
//! adapter; its `PositionCommands` side runs on one worker thread, one
//! command at a time in submission order, behind a queue of
//! [`POSITIONS_QUEUE_BOUND`] waiting commands.
//!
//! `submit` refuses synchronously, with the reason as a string, when no
//! service is configured, the service could not start, the queue is full, or
//! the worker has stopped. An accepted command is answered by exactly one
//! `DataEvent::Command` from the worker: `Ok` when the position system
//! accepted it, the adapter's message when it refused, and
//! `position service panicked` when the transport panicked (the worker then
//! goes on to the next command). The event sink can still refuse that
//! answer. There is no timeout and no retry.
//!
//! Shutdown drops the queue's sender and joins the worker, which finishes the
//! commands already queued first; run it off the UI thread, as every other
//! `DataService` shutdown.

use crate::adapter::{AdapterRegistry, PositionCommands};
use crate::service::{DataEvent, EventSink};
use geode_core::config::{Diagnostic, Severity};
use geode_core::positions::{CommandOutcome, MoveLhuParams, PositionsSpec, noun};
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread::JoinHandle;

/// Commands waiting behind the one in flight. Past this a submission is
/// refused `position service busy` at once rather than waiting.
pub const POSITIONS_QUEUE_BOUND: usize = 8;

/// The refusal when `positions.toml` names no service.
const NOT_CONFIGURED: &str = "no position service configured";

/// The answer a panicking transport's command gets.
const PANICKED: &str = "position service panicked";

/// Drop a service whose adapter is unknown or has no position side, with an
/// error diagnostic at `positions.service.adapter`. Called by geode-app at
/// startup; the survivor goes into `DataServiceConfig::positions`.
pub fn resolve(
    spec: Option<PositionsSpec>,
    adapters: &AdapterRegistry,
) -> (Option<PositionsSpec>, Vec<Diagnostic>) {
    let Some(spec) = spec else {
        return (None, Vec::new());
    };
    let problem = match position_side(&spec.adapter, adapters) {
        Ok(_) => return (Some(spec), Vec::new()),
        Err(problem) => problem,
    };
    (
        None,
        vec![Diagnostic {
            severity: Severity::Error,
            layer: None,
            file: None,
            message: format!("positions: {problem}"),
            path: Some("positions.service.adapter".into()),
        }],
    )
}

/// A fresh position side of the adapter named `adapter`, or why there is
/// none: the one wording `resolve`'s diagnostic and `PositionWorker::spawn`'s
/// refusal share.
fn position_side(
    adapter: &str,
    adapters: &AdapterRegistry,
) -> Result<Box<dyn PositionCommands>, String> {
    match adapters.get(adapter) {
        None => Err(format!("adapter '{adapter}' is not in this build")),
        Some(found) => found
            .positions()
            .ok_or_else(|| format!("adapter '{adapter}' has no position side")),
    }
}

/// The one position-command worker `DataService` owns.
pub struct PositionWorker {
    /// `None` once stopped, or when no worker ever started (`unavailable`
    /// then holds the refusal).
    tx: Mutex<Option<SyncSender<MoveLhuParams>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
    /// The refusal every submission gets when no worker started.
    unavailable: Option<String>,
}

fn work(mut commands: Box<dyn PositionCommands>, jobs: Receiver<MoveLhuParams>, sink: EventSink) {
    while let Ok(job) = jobs.recv() {
        let outcome = CommandOutcome {
            tag: job.tag,
            count: job.positions.len(),
            result: run_job(commands.as_mut(), &job),
            lhu: job.lhu,
        };
        match &outcome.result {
            Ok(()) => tracing::info!(
                target: "geode::ingest",
                "move of {} {} to LHU {} accepted",
                outcome.count,
                noun(outcome.count),
                outcome.lhu
            ),
            Err(e) => tracing::info!(
                target: "geode::ingest",
                "move of {} {} to LHU {} refused: {e}",
                outcome.count,
                noun(outcome.count),
                outcome.lhu
            ),
        }
        let _ = sink(DataEvent::Command(outcome));
    }
}

/// Run one command inside a marked boundary: the transport is foreign code,
/// and its panic fails this command alone.
fn run_job(commands: &mut dyn PositionCommands, job: &MoveLhuParams) -> Result<(), String> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        geode_core::panic::contained(|| commands.move_lhu(&job.positions, &job.lhu))
    })) {
        Ok(result) => result.map_err(|e| e.message),
        Err(payload) => {
            tracing::error!(
                target: "geode::ingest",
                "position service panicked: {}",
                crate::ingest::runner::panic_payload_message(&*payload)
            );
            Err(PANICKED.into())
        }
    }
}

impl PositionWorker {
    /// Start the worker for `spec`'s adapter. No spec, an adapter that is
    /// missing or has no position side (the earlier `resolve` probe took a
    /// separate handle, so this can still fail), or a thread that cannot
    /// start each leave a worker that refuses every submission with the
    /// reason.
    pub fn spawn(
        spec: &Option<PositionsSpec>,
        adapters: &AdapterRegistry,
        sink: EventSink,
    ) -> Self {
        let stopped = |unavailable: String| PositionWorker {
            tx: Mutex::new(None),
            thread: Mutex::new(None),
            unavailable: Some(unavailable),
        };
        let Some(spec) = spec else {
            return stopped(NOT_CONFIGURED.into());
        };
        let commands = match position_side(&spec.adapter, adapters) {
            Ok(commands) => commands,
            Err(why) => {
                tracing::warn!(target: "geode::ingest", "positions: {why}");
                return stopped(format!("position service unavailable: {why}"));
            }
        };
        let (tx, rx) = sync_channel::<MoveLhuParams>(POSITIONS_QUEUE_BOUND);
        let worker_sink = EventSink::clone(&sink);
        match crate::supervise::spawn_supervised("geode-positions".into(), sink, move || {
            work(commands, rx, worker_sink)
        }) {
            Ok(handle) => PositionWorker {
                tx: Mutex::new(Some(tx)),
                thread: Mutex::new(Some(handle)),
                unavailable: None,
            },
            Err(e) => {
                let why = format!("could not start its worker: {e}");
                tracing::warn!(target: "geode::ingest", "positions: {why}");
                stopped(format!("position service unavailable: {why}"))
            }
        }
    }

    /// Queue a command. `Err` is the refusal reason, decided here; nothing
    /// else will answer a refused command. An accepted command is answered
    /// once by the worker.
    pub fn submit(&self, params: MoveLhuParams) -> Result<(), String> {
        let guard = self.tx.lock().unwrap_or_else(|e| e.into_inner());
        let Some(tx) = guard.as_ref() else {
            return Err(self
                .unavailable
                .clone()
                .unwrap_or_else(|| "position service stopped".into()));
        };
        match tx.try_send(params) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err("position service busy".into()),
            Err(TrySendError::Disconnected(_)) => Err("position service stopped".into()),
        }
    }

    /// Close the queue, then join the worker. Queued commands still run and
    /// answer. Idempotent.
    pub fn shutdown(&self) {
        drop(self.tx.lock().unwrap_or_else(|e| e.into_inner()).take());
        let handle = self.thread.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }
}

impl Drop for PositionWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::adapter::{Adapter, AdapterError, Subscription};
    use std::sync::Arc;
    use std::sync::mpsc::channel;
    use std::time::Duration;

    pub(crate) const ADAPTER: &str = "sim_positions";

    pub(crate) type Calls = Arc<Mutex<Vec<(Vec<String>, String)>>>;

    /// Records every move. An LHU of `ERR` is refused, `PANIC` panics, and
    /// `WAIT` announces itself on `entered` and then waits for `release` (or
    /// its sender's drop). A channel, not a `Barrier`: the test's side waits
    /// with a timeout, so a regression fails instead of hanging.
    struct FakeCommands {
        calls: Calls,
        entered: SyncSender<()>,
        release: Arc<Mutex<Receiver<()>>>,
    }

    impl PositionCommands for FakeCommands {
        fn move_lhu(&mut self, positions: &[String], lhu: &str) -> Result<(), AdapterError> {
            self.calls
                .lock()
                .unwrap()
                .push((positions.to_vec(), lhu.to_string()));
            match lhu {
                "ERR" => Err(AdapterError {
                    message: "unknown position P99".into(),
                }),
                "PANIC" => panic!("the position service fell over"),
                "WAIT" => {
                    let _ = self.entered.send(());
                    let _ = self.release.lock().unwrap().recv();
                    Ok(())
                }
                _ => Ok(()),
            }
        }
    }

    pub(crate) struct FakeAdapter {
        calls: Calls,
        entered: SyncSender<()>,
        release: Arc<Mutex<Receiver<()>>>,
    }

    impl Adapter for FakeAdapter {
        fn name(&self) -> &'static str {
            ADAPTER
        }
        fn subscription(&self) -> Option<Box<dyn Subscription>> {
            None
        }
        fn egress(&self) -> Option<Box<dyn crate::adapter::Egress>> {
            None
        }
        fn positions(&self) -> Option<Box<dyn PositionCommands>> {
            Some(Box::new(FakeCommands {
                calls: Arc::clone(&self.calls),
                entered: self.entered.clone(),
                release: Arc::clone(&self.release),
            }))
        }
    }

    /// A registry holding the fake under [`ADAPTER`], its call log, the
    /// receiver a `WAIT` move announces itself on, and the sender that
    /// releases it.
    pub(crate) fn registry() -> (AdapterRegistry, Calls, Receiver<()>, SyncSender<()>) {
        let calls = Calls::default();
        let (entered, entered_rx) = sync_channel(64);
        let (release, release_rx) = sync_channel(64);
        let mut adapters = AdapterRegistry::default();
        adapters.register(Arc::new(FakeAdapter {
            calls: Arc::clone(&calls),
            entered,
            release: Arc::new(Mutex::new(release_rx)),
        }));
        (adapters, calls, entered_rx, release)
    }

    fn spec() -> Option<PositionsSpec> {
        Some(PositionsSpec {
            adapter: ADAPTER.into(),
        })
    }

    fn event_sink() -> (EventSink, Receiver<DataEvent>) {
        let (tx, rx) = channel();
        let tx = Mutex::new(tx);
        let sink: EventSink = Arc::new(move |e| tx.lock().unwrap().send(e).is_ok());
        (sink, rx)
    }

    pub(crate) fn params(tag: u64, positions: &[&str], lhu: &str) -> MoveLhuParams {
        MoveLhuParams {
            tag,
            positions: positions.iter().map(|p| p.to_string()).collect(),
            lhu: lhu.into(),
        }
    }

    fn next_command(rx: &Receiver<DataEvent>) -> CommandOutcome {
        match rx
            .recv_timeout(Duration::from_secs(5))
            .expect("a command outcome")
        {
            DataEvent::Command(outcome) => outcome,
            other => panic!("not a command outcome: {other:?}"),
        }
    }

    fn assert_silent(rx: &Receiver<DataEvent>) {
        assert!(
            rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "each command answers exactly once"
        );
    }

    #[test]
    fn a_move_is_answered_once() {
        let (adapters, calls, _entered, _release) = registry();
        let (sink, rx) = event_sink();
        let worker = PositionWorker::spawn(&spec(), &adapters, sink);

        assert_eq!(worker.submit(params(3, &["P1", "P2"], "L7")), Ok(()));

        assert_eq!(
            next_command(&rx),
            CommandOutcome {
                tag: 3,
                count: 2,
                lhu: "L7".into(),
                result: Ok(()),
            }
        );
        assert_silent(&rx);
        assert_eq!(
            *calls.lock().unwrap(),
            vec![(vec!["P1".to_string(), "P2".to_string()], "L7".to_string())]
        );
    }

    #[test]
    fn a_transport_error_is_a_refusal() {
        let (adapters, _calls, _entered, _release) = registry();
        let (sink, rx) = event_sink();
        let worker = PositionWorker::spawn(&spec(), &adapters, sink);

        worker.submit(params(4, &["P99"], "ERR")).unwrap();

        assert_eq!(
            next_command(&rx),
            CommandOutcome {
                tag: 4,
                count: 1,
                lhu: "ERR".into(),
                result: Err("unknown position P99".into()),
            }
        );
        assert_silent(&rx);
    }

    #[test]
    fn a_panic_is_answered_not_lost() {
        let (adapters, _calls, _entered, _release) = registry();
        let (sink, rx) = event_sink();
        let worker = PositionWorker::spawn(&spec(), &adapters, sink);

        worker.submit(params(5, &["P1"], "PANIC")).unwrap();
        let outcome = next_command(&rx);
        assert_eq!(
            (outcome.tag, outcome.result),
            (5, Err("position service panicked".into()))
        );
        assert_silent(&rx);

        // The worker survives its transport's panic.
        worker.submit(params(6, &["P1"], "L7")).unwrap();
        assert_eq!(next_command(&rx).result, Ok(()));
    }

    #[test]
    fn without_a_service_a_move_is_refused_synchronously() {
        let (adapters, calls, _entered, _release) = registry();
        let (sink, rx) = event_sink();

        let none = PositionWorker::spawn(&None, &adapters, EventSink::clone(&sink));
        assert_eq!(
            none.submit(params(1, &["P1"], "L7")),
            Err("no position service configured".into())
        );

        let missing = PositionWorker::spawn(
            &Some(PositionsSpec {
                adapter: "nowhere".into(),
            }),
            &adapters,
            EventSink::clone(&sink),
        );
        assert_eq!(
            missing.submit(params(2, &["P1"], "L7")),
            Err("position service unavailable: adapter 'nowhere' is not in this build".into())
        );

        let stopped = PositionWorker::spawn(&spec(), &adapters, sink);
        stopped.shutdown();
        stopped.shutdown();
        assert_eq!(
            stopped.submit(params(3, &["P1"], "L7")),
            Err("position service stopped".into())
        );

        assert_silent(&rx);
        assert!(calls.lock().unwrap().is_empty());
    }

    #[test]
    fn an_adapter_without_a_position_side_refuses_every_move() {
        let (bus, _feed) = crate::adapter::ChannelAdapter::new("demo_bus");
        let mut adapters = AdapterRegistry::default();
        adapters.register(bus);
        let (sink, rx) = event_sink();
        let worker = PositionWorker::spawn(
            &Some(PositionsSpec {
                adapter: "demo_bus".into(),
            }),
            &adapters,
            sink,
        );
        assert_eq!(
            worker.submit(params(1, &["P1"], "L7")),
            Err("position service unavailable: adapter 'demo_bus' has no position side".into())
        );
        assert_silent(&rx);
    }

    #[test]
    fn a_full_queue_is_busy() {
        let (adapters, _calls, entered, release) = registry();
        let (sink, rx) = event_sink();
        let worker = PositionWorker::spawn(&spec(), &adapters, sink);
        // Rebound after `worker` so it drops first: a failing assert then
        // releases the held command before `worker`'s drop joins the thread.
        let release = release;

        // One in flight, held inside the transport...
        worker.submit(params(0, &["P0"], "WAIT")).unwrap();
        entered
            .recv_timeout(Duration::from_secs(5))
            .expect("the held move reaches the transport");
        // ...then the queue fills to its bound...
        for tag in 1..=POSITIONS_QUEUE_BOUND as u64 {
            assert_eq!(worker.submit(params(tag, &["P"], "L7")), Ok(()));
        }
        // ...and the next is refused without waiting, and never answered.
        assert_eq!(
            worker.submit(params(99, &["P"], "L7")),
            Err("position service busy".into())
        );
        assert_silent(&rx);

        release.send(()).unwrap();
        let tags: Vec<u64> = (0..=POSITIONS_QUEUE_BOUND)
            .map(|_| next_command(&rx).tag)
            .collect();
        assert_eq!(tags, (0..=POSITIONS_QUEUE_BOUND as u64).collect::<Vec<_>>());
        assert_silent(&rx);
    }

    #[test]
    fn resolve_drops_an_unknown_adapter_or_one_without_a_position_side() {
        let (adapters, _calls, _entered, _release) = registry();
        assert_eq!(resolve(None, &adapters), (None, Vec::new()));
        assert_eq!(resolve(spec(), &adapters), (spec(), Vec::new()));

        let (kept, diags) = resolve(
            Some(PositionsSpec {
                adapter: "nowhere".into(),
            }),
            &adapters,
        );
        assert_eq!(kept, None);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Error);
        assert_eq!(diags[0].path.as_deref(), Some("positions.service.adapter"));
        assert!(diags[0].message.contains("not in this build"), "{diags:?}");

        let (bus, _feed) = crate::adapter::ChannelAdapter::new("demo_bus");
        let mut with_bus = AdapterRegistry::default();
        with_bus.register(bus);
        let (kept, diags) = resolve(
            Some(PositionsSpec {
                adapter: "demo_bus".into(),
            }),
            &with_bus,
        );
        assert_eq!(kept, None);
        assert!(diags[0].message.contains("no position side"), "{diags:?}");
    }
}
