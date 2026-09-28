//! One vol-evaluation thread with its own queue, a copy of the pricing
//! worker's shape: at most [`VOL_BOUND`] distinct keys wait, a newer
//! batch for a queued key replaces it in place, cancellation drops queued
//! work and stops a running batch at the next job boundary (delivering
//! the jobs done so far). Every model call is a containment boundary: a
//! panic fails that job with its message and the worker continues. An
//! absent model answers every job with the configured reason.

use super::VolConfig;
use geode_core::query::QueryKey;
use geode_core::vol::{VolJob, VolResult, VolSliceOutcome, VolSliceParams};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

pub type VolSink = Arc<dyn Fn(VolSliceOutcome) -> bool + Send + Sync>;

/// Distinct keys that may wait; one batch per key.
pub const VOL_BOUND: usize = 64;

#[derive(Default)]
struct Queue {
    order: VecDeque<QueryKey>,
    pending: HashMap<QueryKey, VolSliceParams>,
    running: Option<QueryKey>,
    cancel_running: bool,
    shutdown: bool,
}

pub struct VolWorker {
    queue: Arc<(Mutex<Queue>, Condvar)>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl VolWorker {
    pub fn spawn(config: VolConfig, sink: VolSink, stop: crate::service::EventSink) -> VolWorker {
        let queue: Arc<(Mutex<Queue>, Condvar)> = Arc::default();
        let thread = {
            let queue = Arc::clone(&queue);
            crate::supervise::spawn_supervised("geode-vol".to_string(), stop, move || {
                run(queue, config, sink)
            })
            .expect("spawn the vol worker")
        };
        VolWorker {
            queue,
            thread: Mutex::new(Some(thread)),
        }
    }

    pub fn request(&self, params: VolSliceParams) -> bool {
        let (lock, cvar) = &*self.queue;
        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
        if q.shutdown {
            return false;
        }
        let key = params.key;
        if let Some(slot) = q.pending.get_mut(&key) {
            *slot = params;
        } else {
            if q.order.len() >= VOL_BOUND {
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

impl Drop for VolWorker {
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

fn run_job(
    config: &VolConfig,
    params: &VolSliceParams,
    index: usize,
    job: &VolJob,
) -> Result<VolResult, String> {
    let Some(model) = &config.model else {
        return Err(config.missing_reason());
    };
    if let VolJob::Slice { document, .. } = job
        && *document >= params.documents.len()
    {
        return Err(format!(
            "job {index} names document {document} of a batch carrying {}",
            params.documents.len()
        ));
    }
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        geode_core::panic::contained(|| match job {
            VolJob::Slice { document, request } => model
                .slice(&params.documents[*document], request)
                .map(VolResult::Slice),
            VolJob::Map(request) => model.coordinates(request).map(VolResult::Map),
        })
    }));
    match outcome {
        Ok(Ok(r)) => Ok(r),
        Ok(Err(e)) => Err(e.0),
        Err(payload) => {
            let message = panic_message(&payload);
            tracing::warn!(
                target: "geode::vol",
                "vol model panicked on job {index} of key {}: {message}",
                params.key.0
            );
            Err(format!("vol model panicked: {message}"))
        }
    }
}

fn run(queue: Arc<(Mutex<Queue>, Condvar)>, config: VolConfig, sink: VolSink) {
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
        let mut results = Vec::with_capacity(params.jobs.len());
        let mut failures = 0usize;
        for (index, job) in params.jobs.iter().enumerate() {
            {
                let q = lock.lock().unwrap_or_else(|e| e.into_inner());
                if q.cancel_running || q.shutdown {
                    break;
                }
            }
            let result = run_job(&config, &params, index, job);
            if result.is_err() {
                failures += 1;
            }
            results.push(result);
        }
        {
            let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
            q.running = None;
            q.cancel_running = false;
        }
        tracing::debug!(
            target: "geode::vol",
            "evaluated {} of {} job(s) for key {} tag {} in {:?} ({failures} failed)",
            results.len(), params.jobs.len(), params.key.0, params.tag, started.elapsed()
        );
        let delivered = sink(VolSliceOutcome {
            key: params.key,
            tag: params.tag,
            submitted: params.submitted,
            results,
        });
        if !delivered && !refusal_logged {
            refusal_logged = true;
            tracing::warn!(
                target: "geode::vol",
                "a vol outcome for key {} was not delivered; further refusals are not logged",
                params.key.0
            );
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use geode_core::document::DocumentRows;
    use geode_core::vol::{
        Coordinate, Grid, MapRequest, SliceRequest, SliceResult, VolError, VolJob, VolModel,
        VolResult,
    };
    use std::sync::Mutex;
    use std::sync::mpsc::channel;
    use std::time::{Duration, Instant};

    /// Slices anything but an expiry in 1999 (an error) or 2000 (a panic),
    /// after `delay`, recording every expiry asked. `coordinates` echoes
    /// the strikes, or panics on a negative forward.
    pub(crate) struct FakeVolModel {
        pub(crate) asked: Arc<Mutex<Vec<String>>>,
        pub(crate) delay: Duration,
    }
    impl VolModel for FakeVolModel {
        fn name(&self) -> &str {
            "fake"
        }
        fn kind(&self) -> &str {
            "cvi_params"
        }
        fn slice(&self, _: &DocumentRows, req: &SliceRequest) -> Result<SliceResult, VolError> {
            self.asked.lock().unwrap().push(req.expiry.to_string());
            std::thread::sleep(self.delay);
            match req.expiry.format("%Y").to_string().as_str() {
                "1999" => Err(VolError("refused".into())),
                "2000" => panic!("the fake vol model exploded"),
                _ => Ok(SliceResult {
                    expiry: req.expiry,
                    forward: 100.0,
                    points: Vec::new(),
                    density: None,
                }),
            }
        }
        fn coordinates(&self, req: &MapRequest) -> Result<Vec<f64>, VolError> {
            if req.forward < 0.0 {
                panic!("the fake vol model exploded on a map");
            }
            Ok(req.strikes.clone())
        }
    }

    pub(crate) fn empty_doc() -> Arc<DocumentRows> {
        Arc::new(DocumentRows {
            key: vec!["SPX.Z".into()],
            attributes: Vec::new(),
            axes: Vec::new(),
            values: Vec::new(),
        })
    }

    pub(crate) fn slice_job(document: usize, expiry: &str) -> VolJob {
        VolJob::Slice {
            document,
            request: SliceRequest {
                expiry: chrono::NaiveDate::parse_from_str(expiry, "%Y-%m-%d").unwrap(),
                coordinate: Coordinate::Moneyness,
                grid: Grid::Dense(3),
                density: false,
            },
        }
    }

    pub(crate) fn map_job(forward: f64) -> VolJob {
        VolJob::Map(MapRequest {
            expiry: chrono::NaiveDate::from_ymd_opt(2026, 12, 18).unwrap(),
            as_of: chrono::NaiveDate::from_ymd_opt(2026, 9, 18).unwrap(),
            forward,
            coordinate: Coordinate::Strike,
            strikes: vec![90.0, 110.0],
            vols: vec![0.2, 0.2],
        })
    }

    /// One document, one `Slice` job per expiry.
    pub(crate) fn params(key: u64, tag: u64, expiries: &[&str]) -> VolSliceParams {
        VolSliceParams {
            key: QueryKey(key),
            tag,
            submitted: Instant::now(),
            documents: vec![empty_doc()],
            jobs: expiries.iter().map(|e| slice_job(0, e)).collect(),
        }
    }

    fn worker(
        delay: Duration,
    ) -> (
        VolWorker,
        Arc<Mutex<Vec<String>>>,
        std::sync::mpsc::Receiver<VolSliceOutcome>,
    ) {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = channel();
        let sink: VolSink = Arc::new(move |o| tx.send(o).is_ok());
        let w = VolWorker::spawn(
            VolConfig::with(Arc::new(FakeVolModel {
                asked: asked.clone(),
                delay,
            })),
            sink,
            crate::supervise::unwatched(),
        );
        (w, asked, rx)
    }

    fn next(rx: &std::sync::mpsc::Receiver<VolSliceOutcome>) -> VolSliceOutcome {
        rx.recv_timeout(Duration::from_secs(10))
            .expect("an outcome")
    }

    #[test]
    fn a_batch_is_evaluated_job_by_job_and_answered_under_its_key_and_tag() {
        let (w, _, rx) = worker(Duration::ZERO);
        let mut p = params(7, 3, &["2026-10-16", "1999-01-01"]);
        p.jobs.push(map_job(100.0));
        let submitted = p.submitted;
        assert!(w.request(p));
        let o = next(&rx);
        assert_eq!((o.key, o.tag), (QueryKey(7), 3));
        assert_eq!(o.submitted, submitted);
        assert_eq!(o.results.len(), 3);
        assert!(matches!(o.results[0], Ok(VolResult::Slice(_))));
        assert_eq!(o.results[1].as_ref().unwrap_err(), "refused");
        assert_eq!(o.results[2], Ok(VolResult::Map(vec![90.0, 110.0])));
        w.shutdown();
    }

    #[test]
    fn latest_wins_per_key_while_queued() {
        let (w, asked, rx) = worker(Duration::from_millis(50));
        assert!(w.request(params(1, 1, &["2026-01-01"])));
        assert!(w.request(params(9, 1, &["2026-02-02"])));
        assert!(w.request(params(9, 2, &["2026-03-03"])));
        let first = next(&rx);
        assert_eq!(first.key, QueryKey(1));
        let second = next(&rx);
        assert_eq!((second.key, second.tag), (QueryKey(9), 2));
        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "only two outcomes"
        );
        assert!(
            !asked.lock().unwrap().iter().any(|e| e == "2026-02-02"),
            "{:?}",
            asked.lock().unwrap()
        );
        w.shutdown();
    }

    #[test]
    fn cancel_drops_a_queued_batch_and_stops_a_running_one_at_the_job_boundary() {
        let (w, _, rx) = worker(Duration::from_millis(40));
        assert!(w.request(params(
            1,
            1,
            &[
                "2026-01-01",
                "2026-02-01",
                "2026-03-01",
                "2026-04-01",
                "2026-05-01"
            ]
        )));
        assert!(w.request(params(2, 1, &["2026-06-01"])));
        std::thread::sleep(Duration::from_millis(60));
        w.cancel(QueryKey(2));
        w.cancel(QueryKey(1));
        let o = next(&rx);
        assert_eq!(o.key, QueryKey(1));
        assert!(o.results.len() < 5, "stopped early: {}", o.results.len());
        assert!(!o.results.is_empty(), "the jobs already done are delivered");
        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "key 2 never ran"
        );
        w.shutdown();
    }

    #[test]
    fn the_queue_is_bounded_by_distinct_keys_and_a_stopped_worker_refuses() {
        let (w, _, rx) = worker(Duration::from_millis(200));
        assert!(w.request(params(0, 1, &["2026-01-01"]))); // runs, freeing the queue
        std::thread::sleep(Duration::from_millis(20));
        for k in 1..=VOL_BOUND as u64 {
            assert!(w.request(params(k, 1, &["2026-01-01"])), "key {k} fits");
        }
        assert!(
            !w.request(params(VOL_BOUND as u64 + 1, 1, &["2026-01-01"])),
            "one over the bound is refused"
        );
        assert!(
            w.request(params(1, 2, &["2026-01-01"])),
            "a replacement for a queued key always fits"
        );
        w.shutdown();
        assert!(
            !w.request(params(500, 1, &["2026-01-01"])),
            "a stopped worker refuses"
        );
        drop(rx);
    }

    #[test]
    fn a_panicking_job_fails_alone_and_the_worker_survives() {
        let (w, _, rx) = worker(Duration::ZERO);
        let mut p = params(3, 1, &["2000-01-01", "2026-01-01"]);
        p.jobs.push(map_job(-1.0));
        assert!(w.request(p));
        let o = next(&rx);
        assert_eq!(o.results.len(), 3);
        assert_eq!(
            o.results[0].as_ref().unwrap_err(),
            "vol model panicked: the fake vol model exploded"
        );
        assert!(o.results[1].is_ok());
        assert_eq!(
            o.results[2].as_ref().unwrap_err(),
            "vol model panicked: the fake vol model exploded on a map"
        );
        assert!(w.request(params(4, 1, &["2026-02-02"])));
        assert_eq!(next(&rx).key, QueryKey(4));
        w.shutdown();
    }

    #[test]
    fn a_job_naming_a_missing_document_errors_without_stopping_the_batch() {
        let (w, asked, rx) = worker(Duration::ZERO);
        let mut p = params(5, 1, &["2026-01-01"]);
        p.jobs.insert(0, slice_job(1, "2026-09-09"));
        assert!(w.request(p));
        let o = next(&rx);
        assert_eq!(o.results.len(), 2);
        assert_eq!(
            o.results[0].as_ref().unwrap_err(),
            "job 0 names document 1 of a batch carrying 1"
        );
        assert!(o.results[1].is_ok());
        assert_eq!(
            asked.lock().unwrap().as_slice(),
            ["2026-01-01"],
            "the model never saw the bad job"
        );
        w.shutdown();
    }

    #[test]
    fn a_missing_model_answers_every_job_with_the_configured_reason() {
        let (tx, rx) = channel();
        let sink: VolSink = Arc::new(move |o| tx.send(o).is_ok());
        let w = VolWorker::spawn(
            VolConfig::missing("vendor"),
            sink,
            crate::supervise::unwatched(),
        );
        let mut p = params(6, 1, &["2026-01-01"]);
        p.jobs.push(map_job(100.0));
        assert!(w.request(p));
        let o = next(&rx);
        assert_eq!(o.results.len(), 2);
        for r in &o.results {
            assert_eq!(
                r.as_ref().unwrap_err(),
                "vol model \"vendor\" is not built into this binary"
            );
        }
        w.shutdown();
    }
}
