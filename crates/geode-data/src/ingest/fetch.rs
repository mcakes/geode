//! The fetch worker (timeseries spec §5.4): one thread per fetch source
//! that owns the adapter's `Fetch` and runs its blocking calls off every
//! other thread. It knows nothing about storage: an outcome goes to the
//! sink the service built, which submits rows to the ingest runner,
//! reports failures on the load lane, and stores identities.

use crate::adapter::{AdapterError, Fetch, FetchRequest, SeriesRows};
use crate::store::series::Span;
use chrono::{DateTime, Utc};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread::JoinHandle;

/// Queued requests per source before `request` refuses. A trader's chart
/// asks for a handful of gaps at a time; sixty-four is a burst.
pub const FETCH_BOUND: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchWork {
    Span {
        identity: String,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    },
    Identities,
}

#[derive(Debug)]
pub enum FetchOutcome {
    Fetched {
        identity: String,
        rows: SeriesRows,
        span: Span,
        /// Non-finite values dropped before the rows were handed on.
        dropped: usize,
    },
    Failed {
        identity: String,
        reason: String,
    },
    Identities(Option<Vec<String>>),
}

/// Where an outcome goes. Called on the worker's own thread, so it must
/// not block — the service's sink submits into the ingest runner (a
/// `push_back` under a lock) and sends one event, nothing more.
pub type FetchOutcomeSink = Arc<dyn Fn(FetchOutcome) + Send + Sync>;

pub struct FetchWorker {
    source: String,
    /// `None` once [`FetchWorker::shutdown`] has taken it, which is what
    /// ends `run`'s `recv` loop and what makes a later `request` refuse.
    tx: Option<SyncSender<FetchWork>>,
    thread: Option<JoinHandle<()>>,
}

impl FetchWorker {
    pub fn spawn(
        source: &str,
        fetch: Box<dyn Fetch>,
        sink: FetchOutcomeSink,
    ) -> Result<FetchWorker, AdapterError> {
        let (tx, rx) = sync_channel(FETCH_BOUND);
        let name = format!("geode-fetch-{source}");
        let thread = std::thread::Builder::new()
            .name(name.clone())
            .spawn(move || run(fetch, rx, sink))
            .map_err(|e| AdapterError {
                message: format!("spawning {name}: {e}"),
            })?;
        Ok(FetchWorker {
            source: source.to_string(),
            tx: Some(tx),
            thread: Some(thread),
        })
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    /// Queue one unit of work. `false` means it was not queued — the
    /// bounded queue is full or the worker is gone — and the caller
    /// reports that as the request's outcome. Never blocks.
    pub fn request(&self, work: FetchWork) -> bool {
        match &self.tx {
            Some(tx) => !matches!(
                tx.try_send(work),
                Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_))
            ),
            None => false,
        }
    }

    /// Drop the sender (so `run`'s `recv` ends once the queue drains) and
    /// join. Idempotent; also on `Drop`.
    pub fn shutdown(&mut self) {
        self.tx.take();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for FetchWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The worker thread: one blocking adapter call per queued unit of work,
/// under `contained` like every other background boundary in this crate
/// — a vendor client that panics takes its own fetch down, logs, and
/// leaves the thread (and the app) running.
fn run(mut fetch: Box<dyn Fetch>, rx: Receiver<FetchWork>, sink: FetchOutcomeSink) {
    while let Ok(work) = rx.recv() {
        let identity = match &work {
            FetchWork::Span { identity, .. } => Some(identity.clone()),
            FetchWork::Identities => None,
        };
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            geode_core::panic::contained(|| match work {
                FetchWork::Identities => FetchOutcome::Identities(fetch.catalogue()),
                FetchWork::Span { identity, from, to } => {
                    let req = FetchRequest {
                        identity: identity.clone(),
                        from,
                        to,
                    };
                    match fetch.fetch(&req).and_then(|mut rows| {
                        rows.validate()?;
                        let dropped = rows.drop_non_finite();
                        Ok((rows, dropped))
                    }) {
                        Ok((rows, dropped)) => {
                            if dropped > 0 {
                                tracing::warn!(
                                    target: "geode::ingest",
                                    "fetch of {identity}: {dropped} non-finite value(s) dropped"
                                );
                            }
                            FetchOutcome::Fetched {
                                identity,
                                rows,
                                span: (from, to),
                                dropped,
                            }
                        }
                        Err(e) => FetchOutcome::Failed {
                            identity,
                            reason: e.message,
                        },
                    }
                }
            })
        }));
        match outcome {
            Ok(o) => sink(o),
            Err(payload) => {
                let message = crate::ingest::runner::panic_payload_message(payload.as_ref());
                tracing::error!(target: "geode::ingest", "a fetch panicked: {message}");
                if let Some(identity) = identity {
                    sink(FetchOutcome::Failed {
                        identity,
                        reason: format!("fetch panicked: {message}"),
                    });
                }
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::adapter::{AdapterError, Fetch, FetchRequest, SeriesRows};
    use std::sync::Mutex;
    use std::sync::mpsc::channel;

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    /// Answers `n` one-minute bars from `from` for any identity but
    /// "broken", and counts its calls.
    ///
    /// With `fail_once`, "broken" fails only the FIRST time it is asked
    /// for and answers bars on every later call — the shape a
    /// clear-on-success test needs, since a lane that never recovers can
    /// only ever show the failure half. The count is taken from the
    /// shared `calls` log rather than a field of its own, because
    /// `Adapter::fetch` hands out a fresh `FakeFetch` per source while
    /// `calls` is the one thing every copy shares.
    pub(crate) struct FakeFetch {
        pub(crate) calls: Arc<Mutex<Vec<FetchRequest>>>,
        pub(crate) n: usize,
        pub(crate) catalogue: Option<Vec<String>>,
        pub(crate) fail_once: bool,
    }

    impl Fetch for FakeFetch {
        fn fetch(&mut self, req: &FetchRequest) -> Result<SeriesRows, AdapterError> {
            let asked_before = {
                let mut calls = self.calls.lock().unwrap_or_else(|e| e.into_inner());
                let asked_before = calls.iter().filter(|c| c.identity == req.identity).count();
                calls.push(req.clone());
                asked_before
            };
            if req.identity == "broken" && !(self.fail_once && asked_before > 0) {
                return Err(AdapterError {
                    message: "no such symbol".into(),
                });
            }
            Ok(SeriesRows {
                ts: (0..self.n)
                    .map(|i| req.from + chrono::Duration::minutes(i as i64))
                    .collect(),
                value: (0..self.n)
                    .map(|i| if i == 1 { f64::NAN } else { i as f64 })
                    .collect(),
            })
        }

        fn catalogue(&mut self) -> Option<Vec<String>> {
            self.catalogue.clone()
        }
    }

    #[test]
    fn a_span_request_yields_fetched_rows_with_non_finite_values_dropped() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = channel();
        let sink: FetchOutcomeSink = Arc::new(move |o| {
            let _ = tx.send(o);
        });
        let mut w = FetchWorker::spawn(
            "demo_kdb",
            Box::new(FakeFetch {
                calls: calls.clone(),
                n: 3,
                catalogue: None,
                fail_once: false,
            }),
            sink,
        )
        .unwrap();
        assert!(w.request(FetchWork::Span {
            identity: "SPX.close".into(),
            from: ts("2026-01-05T00:00:00Z"),
            to: ts("2026-01-06T00:00:00Z"),
        }));
        match rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap() {
            FetchOutcome::Fetched {
                identity,
                rows,
                span,
                dropped,
            } => {
                assert_eq!(identity, "SPX.close");
                assert_eq!(rows.len(), 2);
                assert_eq!(dropped, 1);
                assert_eq!(
                    span,
                    (ts("2026-01-05T00:00:00Z"), ts("2026-01-06T00:00:00Z"))
                );
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(calls.lock().unwrap().len(), 1);
        w.shutdown();
    }

    #[test]
    fn an_adapter_error_is_a_failed_outcome_for_that_identity() {
        let (tx, rx) = channel();
        let sink: FetchOutcomeSink = Arc::new(move |o| {
            let _ = tx.send(o);
        });
        let mut w = FetchWorker::spawn(
            "demo_kdb",
            Box::new(FakeFetch {
                calls: Default::default(),
                n: 1,
                catalogue: None,
                fail_once: false,
            }),
            sink,
        )
        .unwrap();
        w.request(FetchWork::Span {
            identity: "broken".into(),
            from: ts("2026-01-05T00:00:00Z"),
            to: ts("2026-01-06T00:00:00Z"),
        });
        match rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap() {
            FetchOutcome::Failed { identity, reason } => {
                assert_eq!(identity, "broken");
                assert_eq!(reason, "no such symbol");
            }
            other => panic!("{other:?}"),
        }
        w.shutdown();
    }

    #[test]
    fn identities_are_answered_and_shutdown_joins() {
        let (tx, rx) = channel();
        let sink: FetchOutcomeSink = Arc::new(move |o| {
            let _ = tx.send(o);
        });
        let mut w = FetchWorker::spawn(
            "demo_kdb",
            Box::new(FakeFetch {
                calls: Default::default(),
                n: 1,
                catalogue: Some(vec!["VIX".into(), "SPX.close".into()]),
                fail_once: false,
            }),
            sink,
        )
        .unwrap();
        w.request(FetchWork::Identities);
        match rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap() {
            FetchOutcome::Identities(Some(ids)) => assert_eq!(ids, vec!["VIX", "SPX.close"]),
            other => panic!("{other:?}"),
        }
        w.shutdown();
        assert!(
            !w.request(FetchWork::Identities),
            "a stopped worker refuses"
        );
    }
}
