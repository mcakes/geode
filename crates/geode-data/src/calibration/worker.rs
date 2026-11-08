//! One calibration thread with a two-lane queue. Work waits per
//! `(QueryKey, Lane)` slot: the background lane holds at most one `Run`
//! and a newer one replaces it; the user lane is a queue of a tile's
//! actions in which a newer replaceable action (`Evaluate`) supersedes a
//! replaceable one at the tail, while a `Seed` or `Publish` is never
//! replaced and runs exactly once. At most [`CALIBRATION_BOUND`] distinct
//! slots wait; a request for a slot already waiting always fits. Slots
//! run in the order they were filled, one request each turn, so a tile's
//! user action is never starved by another tile's runs. Cancellation by
//! key drops both lanes and withholds the outcome of a request already
//! running; the engine call itself is not interrupted. Every engine call
//! is a containment boundary: a panic fails that request with its message
//! and the worker continues. An absent engine answers every request with
//! the configured reason.

use super::CalibratorConfig;
use geode_core::calibration::{
    CalibrationOutcome, CalibrationRequest, Lane, RunAction, StagePayload, StageResult, StageStatus,
};
use geode_core::query::QueryKey;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub type CalibrationSink = Arc<dyn Fn(CalibrationOutcome) -> bool + Send + Sync>;

/// Distinct `(key, lane)` slots that may wait.
pub const CALIBRATION_BOUND: usize = 64;

/// The stage a failure that never reached the engine is reported under.
pub(crate) const REQUEST_STAGE: &str = "request";

type Slot = (QueryKey, Lane);

#[derive(Default)]
struct Queue {
    /// Slots with work, each once, in the order they went from empty to
    /// non-empty.
    order: VecDeque<Slot>,
    /// Never holds an empty deque: a slot is removed when its last
    /// request is taken.
    pending: HashMap<Slot, VecDeque<CalibrationRequest>>,
    running: Option<QueryKey>,
    cancel_running: bool,
    shutdown: bool,
}

pub struct CalibrationWorker {
    queue: Arc<(Mutex<Queue>, Condvar)>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl CalibrationWorker {
    pub fn spawn(
        config: CalibratorConfig,
        sink: CalibrationSink,
        stop: crate::service::EventSink,
    ) -> CalibrationWorker {
        let queue: Arc<(Mutex<Queue>, Condvar)> = Arc::default();
        let thread = {
            let queue = Arc::clone(&queue);
            crate::supervise::spawn_supervised("geode-calibration".to_string(), stop, move || {
                run(queue, config, sink)
            })
            .expect("spawn the calibration worker")
        };
        CalibrationWorker {
            queue,
            thread: Mutex::new(Some(thread)),
        }
    }

    /// Queue `request`; `false` when the worker has stopped or the bound of
    /// distinct slots is reached by a request for a slot not yet waiting.
    pub fn request(&self, request: CalibrationRequest) -> bool {
        let (lock, cvar) = &*self.queue;
        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
        if q.shutdown {
            return false;
        }
        let lane = request.action.lane();
        let slot: Slot = (request.key, lane);
        if let Some(waiting) = q.pending.get_mut(&slot) {
            match lane {
                Lane::Background => {
                    waiting.clear();
                    waiting.push_back(request);
                }
                Lane::User => {
                    let replaces = waiting.back().is_some_and(|b| b.action.replaceable())
                        && request.action.replaceable();
                    if replaces {
                        if let Some(back) = waiting.back_mut() {
                            *back = request;
                        }
                    } else {
                        waiting.push_back(request);
                    }
                }
            }
        } else {
            if q.order.len() >= CALIBRATION_BOUND {
                return false;
            }
            q.order.push_back(slot);
            q.pending.insert(slot, VecDeque::from([request]));
        }
        cvar.notify_all();
        true
    }

    /// Drop every queued request for `key` in both lanes, and withhold the
    /// outcome of one already running.
    pub fn cancel(&self, key: QueryKey) {
        let (lock, _) = &*self.queue;
        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut dropped = false;
        for lane in [Lane::Background, Lane::User] {
            dropped |= q.pending.remove(&(key, lane)).is_some();
        }
        if dropped {
            q.order.retain(|(k, _)| *k != key);
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

impl Drop for CalibrationWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_string())
}

/// The terminal outcome for a request the engine never answered: one
/// [`REQUEST_STAGE`] stage `Failed(reason)` with the reason as its
/// summary, carrying the request's address and action so the tile routes
/// and paints it as any other answer.
pub(crate) fn request_failed(
    key: QueryKey,
    tag: u64,
    submitted: Instant,
    action: RunAction,
    reason: String,
) -> CalibrationOutcome {
    CalibrationOutcome {
        key,
        tag,
        submitted,
        action,
        stages: vec![StageResult {
            name: REQUEST_STAGE.to_string(),
            status: StageStatus::Failed(reason.clone()),
            ran_at: None,
            elapsed: Duration::ZERO,
            summary: reason,
            payload: StagePayload::None,
        }],
        publish_rows: None,
    }
}

fn run_request(config: &CalibratorConfig, request: &CalibrationRequest) -> CalibrationOutcome {
    let failed = |reason: String| {
        request_failed(
            request.key,
            request.tag,
            request.submitted,
            request.action.clone(),
            reason,
        )
    };
    let Some(engine) = &config.engine else {
        return failed(config.missing_reason());
    };
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        geode_core::panic::contained(|| engine.run(request))
    }));
    match outcome {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => failed(e.0),
        Err(payload) => {
            let message = panic_message(&payload);
            tracing::warn!(
                target: "geode::calibration",
                "calibrator panicked on key {} tag {}: {message}",
                request.key.0, request.tag
            );
            failed(format!("calibrator panicked: {message}"))
        }
    }
}

fn run(queue: Arc<(Mutex<Queue>, Condvar)>, config: CalibratorConfig, sink: CalibrationSink) {
    let (lock, cvar) = &*queue;
    let mut refusal_logged = false;
    loop {
        let request = {
            let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if q.shutdown {
                    return;
                }
                if let Some(slot) = q.order.pop_front() {
                    let Some(waiting) = q.pending.get_mut(&slot) else {
                        continue;
                    };
                    let Some(request) = waiting.pop_front() else {
                        q.pending.remove(&slot);
                        continue;
                    };
                    if waiting.is_empty() {
                        q.pending.remove(&slot);
                    } else {
                        q.order.push_back(slot);
                    }
                    q.running = Some(slot.0);
                    q.cancel_running = false;
                    break request;
                }
                q = cvar.wait(q).unwrap_or_else(|e| e.into_inner());
            }
        };
        let started = Instant::now();
        let outcome = run_request(&config, &request);
        let cancelled = {
            let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
            q.running = None;
            std::mem::take(&mut q.cancel_running)
        };
        tracing::debug!(
            target: "geode::calibration",
            "ran {:?} for key {} tag {} in {:?} ({} stage(s){})",
            request.action, request.key.0, request.tag, started.elapsed(), outcome.stages.len(),
            if cancelled { ", cancelled" } else { "" }
        );
        if cancelled {
            continue;
        }
        let delivered = sink(outcome);
        if !delivered && !refusal_logged {
            refusal_logged = true;
            tracing::warn!(
                target: "geode::calibration",
                "a calibration outcome for key {} was not delivered; further refusals are not logged",
                request.key.0
            );
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::calibration::CalibratorConfig;
    use chrono::Utc;
    use geode_core::calibration::{
        CalibrationError, CalibrationOutcome, CalibrationRequest, Calibrator, Pipeline,
        PublishSource, RunAction, RunMode, StagePayload, StageResult, StageStatus,
    };
    use geode_core::query::QueryKey;
    use std::sync::mpsc::channel;
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::{Duration, Instant};

    /// Holds the engine inside `run` while closed, so a test can queue
    /// requests behind a running one and see what the queue did to them.
    #[derive(Default)]
    pub(crate) struct Gate {
        open: Mutex<bool>,
        cv: Condvar,
    }

    impl Gate {
        pub(crate) fn open() -> Arc<Gate> {
            Arc::new(Gate {
                open: Mutex::new(true),
                cv: Condvar::new(),
            })
        }

        pub(crate) fn closed() -> Arc<Gate> {
            Arc::new(Gate::default())
        }

        pub(crate) fn release(&self) {
            *self.open.lock().unwrap() = true;
            self.cv.notify_all();
        }

        /// Bounded: a test that fails while the gate is closed never
        /// releases it, and the worker's `Drop` joins this thread, so an
        /// unbounded wait would turn that failure into a hang.
        fn wait(&self) {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut open = self.open.lock().unwrap();
            while !*open {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return;
                }
                open = self.cv.wait_timeout(open, left).unwrap().0;
            }
        }
    }

    /// Records every request it is asked (key, tag, action), in order,
    /// before waiting at the gate; answers one Ok stage per request.
    /// An underlying of `error` answers `Err`; `panic` panics.
    pub(crate) struct FakeCalibrator {
        pub(crate) seen: Arc<Mutex<Vec<(u64, u64, RunAction)>>>,
        pub(crate) gate: Arc<Gate>,
    }

    impl Calibrator for FakeCalibrator {
        fn name(&self) -> &str {
            "fake"
        }

        fn run(
            &self,
            request: &CalibrationRequest,
        ) -> Result<CalibrationOutcome, CalibrationError> {
            self.seen
                .lock()
                .unwrap()
                .push((request.key.0, request.tag, request.action.clone()));
            self.gate.wait();
            match request.underlying.as_str() {
                "error" => Err(CalibrationError("the fake engine refused".into())),
                "panic" => panic!("the fake engine exploded"),
                _ => Ok(CalibrationOutcome {
                    key: request.key,
                    tag: request.tag,
                    submitted: request.submitted,
                    action: request.action.clone(),
                    stages: vec![StageResult {
                        name: "fake".into(),
                        status: StageStatus::Ok,
                        ran_at: Some(Utc::now()),
                        elapsed: Duration::ZERO,
                        summary: "ran".into(),
                        payload: StagePayload::None,
                    }],
                    publish_rows: None,
                }),
            }
        }
    }

    pub(crate) fn run() -> RunAction {
        RunAction::Run { from: None }
    }

    pub(crate) fn evaluate(p: f64) -> RunAction {
        RunAction::Evaluate {
            stage: "fit".into(),
            params: vec![p],
        }
    }

    pub(crate) fn seed() -> RunAction {
        RunAction::Seed {
            stage: "fit".into(),
            params: vec![0.5],
        }
    }

    pub(crate) fn publish() -> RunAction {
        RunAction::Publish {
            from: PublishSource::Filter,
        }
    }

    /// A request over an empty pipeline; the fake engine reads only the
    /// key, tag, action and underlying.
    pub(crate) fn request(key: u64, tag: u64, action: RunAction) -> CalibrationRequest {
        request_for(key, tag, action, "SPX")
    }

    pub(crate) fn request_for(
        key: u64,
        tag: u64,
        action: RunAction,
        underlying: &str,
    ) -> CalibrationRequest {
        CalibrationRequest {
            key: QueryKey(key),
            tag,
            submitted: Instant::now(),
            pipeline: Pipeline {
                name: "p".into(),
                title: "p".into(),
                run: RunMode::OnInput,
                stages: Vec::new(),
            },
            underlying: underlying.to_string(),
            inputs: Vec::new(),
            action,
        }
    }

    type Seen = Arc<Mutex<Vec<(u64, u64, RunAction)>>>;

    fn worker(
        gate: Arc<Gate>,
    ) -> (
        CalibrationWorker,
        Seen,
        std::sync::mpsc::Receiver<CalibrationOutcome>,
    ) {
        let seen: Seen = Arc::default();
        let (tx, rx) = channel();
        let sink: CalibrationSink = Arc::new(move |o| tx.send(o).is_ok());
        let w = CalibrationWorker::spawn(
            CalibratorConfig::with(Arc::new(FakeCalibrator {
                seen: seen.clone(),
                gate,
            })),
            sink,
            crate::supervise::unwatched(),
        );
        (w, seen, rx)
    }

    fn next(rx: &std::sync::mpsc::Receiver<CalibrationOutcome>) -> CalibrationOutcome {
        rx.recv_timeout(Duration::from_secs(10))
            .expect("an outcome")
    }

    fn no_more(rx: &std::sync::mpsc::Receiver<CalibrationOutcome>) {
        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "no further outcome"
        );
    }

    /// Submits a blocker on key 0 behind a closed gate and waits until the
    /// engine holds it, so everything queued after it waits in the queue.
    fn blocked() -> (
        CalibrationWorker,
        Seen,
        std::sync::mpsc::Receiver<CalibrationOutcome>,
        Arc<Gate>,
    ) {
        let gate = Gate::closed();
        let (w, seen, rx) = worker(Arc::clone(&gate));
        assert!(w.request(request(0, 0, run())));
        let deadline = Instant::now() + Duration::from_secs(10);
        while seen.lock().unwrap().is_empty() {
            assert!(Instant::now() < deadline, "the blocker never started");
            std::thread::sleep(Duration::from_millis(1));
        }
        (w, seen, rx, gate)
    }

    /// `seen` without the blocker, as (key, tag, action).
    fn seen_after_blocker(seen: &Seen) -> Vec<(u64, u64, RunAction)> {
        seen.lock().unwrap().iter().skip(1).cloned().collect()
    }

    #[test]
    fn a_request_is_answered_under_its_key_tag_and_action() {
        let (w, _, rx) = worker(Gate::open());
        let r = request(7, 3, evaluate(0.1));
        let submitted = r.submitted;
        assert!(w.request(r));
        let o = next(&rx);
        assert_eq!((o.key, o.tag), (QueryKey(7), 3));
        assert_eq!(o.submitted, submitted);
        assert_eq!(o.action, evaluate(0.1));
        assert_eq!(o.stages.len(), 1);
        assert_eq!(o.stages[0].status, StageStatus::Ok);
        w.shutdown();
    }

    #[test]
    fn a_newer_run_replaces_a_queued_run_for_the_same_tile() {
        let (w, seen, rx, gate) = blocked();
        assert!(w.request(request(1, 1, run())));
        assert!(w.request(request(1, 2, run())));
        gate.release();
        assert_eq!(next(&rx).key, QueryKey(0));
        let o = next(&rx);
        assert_eq!((o.key, o.tag), (QueryKey(1), 2));
        no_more(&rx);
        assert_eq!(seen_after_blocker(&seen), vec![(1, 2, run())]);
        w.shutdown();
    }

    #[test]
    fn a_newer_evaluate_replaces_a_queued_evaluate_at_the_tail() {
        let (w, seen, rx, gate) = blocked();
        assert!(w.request(request(1, 1, evaluate(0.1))));
        assert!(w.request(request(1, 2, evaluate(0.2))));
        gate.release();
        assert_eq!(next(&rx).key, QueryKey(0));
        let o = next(&rx);
        assert_eq!((o.key, o.tag), (QueryKey(1), 2));
        assert_eq!(o.action, evaluate(0.2));
        no_more(&rx);
        assert_eq!(seen_after_blocker(&seen), vec![(1, 2, evaluate(0.2))]);
        w.shutdown();
    }

    #[test]
    fn a_seed_is_never_replaced_and_runs_exactly_once() {
        let (w, seen, rx, gate) = blocked();
        assert!(w.request(request(1, 1, evaluate(0.1))));
        assert!(w.request(request(1, 2, seed())));
        assert!(w.request(request(1, 3, evaluate(0.3))));
        gate.release();
        assert_eq!(next(&rx).key, QueryKey(0));
        let tags: Vec<u64> = (0..3).map(|_| next(&rx).tag).collect();
        assert_eq!(tags, vec![1, 2, 3]);
        no_more(&rx);
        assert_eq!(
            seen_after_blocker(&seen),
            vec![(1, 1, evaluate(0.1)), (1, 2, seed()), (1, 3, evaluate(0.3))]
        );
        w.shutdown();
    }

    #[test]
    fn a_publish_is_never_replaced_either() {
        let (w, seen, rx, gate) = blocked();
        assert!(w.request(request(1, 1, publish())));
        assert!(w.request(request(1, 2, publish())));
        assert!(w.request(request(1, 3, evaluate(0.3))));
        gate.release();
        assert_eq!(next(&rx).key, QueryKey(0));
        let tags: Vec<u64> = (0..3).map(|_| next(&rx).tag).collect();
        assert_eq!(tags, vec![1, 2, 3]);
        assert_eq!(seen_after_blocker(&seen).len(), 3);
        w.shutdown();
    }

    #[test]
    fn a_run_never_displaces_a_queued_user_action() {
        let (w, seen, rx, gate) = blocked();
        assert!(w.request(request(1, 1, evaluate(0.1))));
        assert!(w.request(request(1, 2, run())));
        assert!(w.request(request(1, 3, seed())));
        gate.release();
        assert_eq!(next(&rx).key, QueryKey(0));
        let mut got: Vec<(u64, RunAction)> = (0..3)
            .map(|_| next(&rx))
            .map(|o| (o.tag, o.action))
            .collect();
        got.sort_by_key(|(tag, _)| *tag);
        assert_eq!(
            got,
            vec![(1, evaluate(0.1)), (2, run()), (3, seed())],
            "the run and both user actions each ran once"
        );
        no_more(&rx);
        assert_eq!(seen_after_blocker(&seen).len(), 3);
        w.shutdown();
    }

    #[test]
    fn slots_run_in_the_order_they_were_filled() {
        // The user slot for key 1 was filled before the background slot
        // for key 2, so it runs first; a slot is one place in the order
        // however many requests wait in it.
        let (w, seen, rx, gate) = blocked();
        assert!(w.request(request(1, 1, run())));
        assert!(w.request(request(1, 2, seed())));
        assert!(w.request(request(2, 1, run())));
        gate.release();
        assert_eq!(next(&rx).key, QueryKey(0));
        let order: Vec<(u64, u64)> = (0..3)
            .map(|_| next(&rx))
            .map(|o| (o.key.0, o.tag))
            .collect();
        assert_eq!(order, vec![(1, 1), (1, 2), (2, 1)]);
        assert_eq!(seen_after_blocker(&seen).len(), 3);
        w.shutdown();
    }

    #[test]
    fn the_queue_is_bounded_by_distinct_slots_and_a_stopped_worker_refuses() {
        let (w, _, rx, gate) = blocked();
        for k in 1..=CALIBRATION_BOUND as u64 {
            assert!(w.request(request(k, 1, run())), "key {k} fits");
        }
        assert!(
            !w.request(request(CALIBRATION_BOUND as u64 + 1, 1, run())),
            "one slot over the bound is refused"
        );
        assert!(
            !w.request(request(1, 1, evaluate(0.1))),
            "the user lane of a queued key is its own slot"
        );
        assert!(
            w.request(request(1, 2, run())),
            "a replacement for a queued slot always fits"
        );
        gate.release();
        w.shutdown();
        assert!(
            !w.request(request(500, 1, run())),
            "a stopped worker refuses"
        );
        drop(rx);
    }

    #[test]
    fn a_panicking_engine_fails_the_request_not_the_worker() {
        let (w, _, rx) = worker(Gate::open());
        assert!(w.request(request_for(3, 1, seed(), "panic")));
        let o = next(&rx);
        assert_eq!((o.key, o.tag), (QueryKey(3), 1));
        assert_eq!(o.action, seed());
        assert_eq!(o.stages.len(), 1);
        assert_eq!(o.stages[0].name, "request");
        assert_eq!(
            o.stages[0].status,
            StageStatus::Failed("calibrator panicked: the fake engine exploded".into())
        );
        assert_eq!(
            o.stages[0].summary,
            "calibrator panicked: the fake engine exploded"
        );
        assert!(o.stages[0].ran_at.is_none());
        assert!(o.publish_rows.is_none());
        assert!(w.request(request(4, 1, run())));
        assert_eq!(next(&rx).key, QueryKey(4));
        w.shutdown();
    }

    #[test]
    fn an_engine_error_fails_the_request_stage_with_its_message() {
        let (w, _, rx) = worker(Gate::open());
        assert!(w.request(request_for(3, 1, run(), "error")));
        let o = next(&rx);
        assert_eq!(o.stages.len(), 1);
        assert_eq!(o.stages[0].name, "request");
        assert_eq!(
            o.stages[0].status,
            StageStatus::Failed("the fake engine refused".into())
        );
        w.shutdown();
    }

    #[test]
    fn a_missing_engine_answers_with_its_reason() {
        let (tx, rx) = channel();
        let sink: CalibrationSink = Arc::new(move |o| tx.send(o).is_ok());
        let w = CalibrationWorker::spawn(
            CalibratorConfig::missing("vendor"),
            sink,
            crate::supervise::unwatched(),
        );
        assert!(w.request(request(6, 1, publish())));
        let o = next(&rx);
        assert_eq!((o.key, o.tag), (QueryKey(6), 1));
        assert_eq!(o.action, publish());
        assert_eq!(o.stages.len(), 1);
        assert_eq!(o.stages[0].name, "request");
        assert_eq!(
            o.stages[0].status,
            StageStatus::Failed(
                "calibration engine \"vendor\" is not built into this binary".into()
            )
        );
        w.shutdown();
    }

    #[test]
    fn cancel_drops_both_lanes_for_the_key() {
        let (w, seen, rx, gate) = blocked();
        assert!(w.request(request(5, 1, run())));
        assert!(w.request(request(5, 2, seed())));
        assert!(w.request(request(6, 1, run())));
        w.cancel(QueryKey(5));
        gate.release();
        assert_eq!(next(&rx).key, QueryKey(0));
        assert_eq!(next(&rx).key, QueryKey(6));
        no_more(&rx);
        assert_eq!(seen_after_blocker(&seen), vec![(6, 1, run())]);
        // The cancelled slots are gone, so key 5 fits again as new slots.
        assert!(w.request(request(5, 3, run())));
        assert_eq!(next(&rx).tag, 3);
        w.shutdown();
    }

    #[test]
    fn a_running_request_whose_key_is_cancelled_is_not_delivered() {
        let (w, _, rx, gate) = blocked();
        w.cancel(QueryKey(0));
        assert!(w.request(request(1, 1, run())));
        gate.release();
        assert_eq!(
            next(&rx).key,
            QueryKey(1),
            "the cancelled blocker never answers"
        );
        no_more(&rx);
        w.shutdown();
    }
}
