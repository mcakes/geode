//! One history worker per history source owns its adapter's blocking
//! `HistoryQuery` calls. An answer is normalized and checked against its ask
//! here, before it is handed on: the service files only rows the request
//! named, under the keys a request can name. The worker owns no storage.

use crate::adapter::{AdapterError, HistoryQuery, HistoryRequest, HistoryRows};
use geode_core::schema::ColumnType;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread::JoinHandle;

/// Queued requests per source before `request` refuses. A watchlist asks
/// for a handful of key sets at a time; sixty-four is a burst.
pub const HISTORY_BOUND: usize = 64;

/// Names one history query from submission to outcome, so a completion can
/// be matched to (or dropped against) the request that is current for its
/// key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Ticket(pub u64);

#[derive(Debug, Clone, PartialEq)]
pub enum HistoryWork {
    /// `value_arity` is the dataset's declared value-column count; the key
    /// arity follows from the worker's key types.
    Query {
        ticket: Ticket,
        request: HistoryRequest,
        value_arity: usize,
    },
    Keys {
        column: String,
    },
}

#[derive(Debug)]
pub enum HistoryWorkOutcome {
    Fetched {
        ticket: Ticket,
        /// The request as the adapter saw it (its table filled in).
        request: HistoryRequest,
        rows: HistoryRows,
        /// Rows with a non-finite value dropped before the rows were handed on.
        dropped: usize,
        /// Taken just before the adapter call: the source can vouch for
        /// nothing later than when it was asked.
        fetched_at: chrono::DateTime<chrono::Utc>,
    },
    Failed {
        ticket: Ticket,
        reason: String,
    },
    Keys {
        column: String,
        values: Option<Vec<String>>,
    },
    /// The key listing panicked; the payload. Its own outcome so the
    /// service can report it against the source instead of a log line.
    KeysPanicked {
        column: String,
        payload: String,
    },
}

/// Receives outcomes on the history thread. Keep callbacks short:
/// processing and shutdown cannot advance until the callback returns.
pub type HistoryWorkOutcomeSink = Arc<dyn Fn(HistoryWorkOutcome) + Send + Sync>;

pub struct HistoryWorker {
    source: String,
    /// `None` once [`HistoryWorker::shutdown`] has taken it, which is what
    /// ends `run`'s `recv` loop and what makes a later `request` refuse.
    tx: Option<SyncSender<HistoryWork>>,
    thread: Option<JoinHandle<()>>,
}

impl HistoryWorker {
    /// `table` fills a request whose table is empty; `key_types` are the
    /// dataset's key column types, which canonicalize the answer's keys.
    pub fn spawn(
        source: &str,
        table: &str,
        key_types: Vec<ColumnType>,
        query: Box<dyn HistoryQuery>,
        sink: HistoryWorkOutcomeSink,
        stop: crate::service::EventSink,
    ) -> Result<HistoryWorker, AdapterError> {
        let (tx, rx) = sync_channel(HISTORY_BOUND);
        let name = format!("geode-history-{source}");
        let table = table.to_string();
        let thread = crate::supervise::spawn_supervised(name.clone(), stop, move || {
            run(query, &table, &key_types, rx, sink)
        })
        .map_err(|e| AdapterError {
            message: format!("spawning {name}: {e}"),
        })?;
        Ok(HistoryWorker {
            source: source.to_string(),
            tx: Some(tx),
            thread: Some(thread),
        })
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    /// Queue one unit of work. `false` means it was not queued (the bounded
    /// queue is full or the worker is gone) and the caller reports that as
    /// the request's outcome. Never blocks.
    pub fn request(&self, work: HistoryWork) -> bool {
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

impl Drop for HistoryWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Normalize, then validate, then drop non-finite rows. The order matters:
/// a source spelling `0.50` for an asked `0.5` must not be refused as not
/// asked for, and a NaN row must not be dropped from an answer that is
/// refused anyway.
fn checked(
    mut rows: HistoryRows,
    key_types: &[ColumnType],
    value_arity: usize,
    request: &HistoryRequest,
) -> Result<(HistoryRows, usize), AdapterError> {
    rows.normalize_keys(key_types)
        .map_err(|message| AdapterError { message })?;
    rows.validate_against(key_types.len(), value_arity, &request.keys)?;
    let dropped = rows.drop_non_finite();
    Ok((rows, dropped))
}

/// The worker thread: one blocking adapter call per queued unit of work,
/// under `contained` like every other background boundary in this crate.
/// A vendor client that panics fails its own ticket, logs, and leaves the
/// thread (and the app) running.
fn run(
    mut query: Box<dyn HistoryQuery>,
    table: &str,
    key_types: &[ColumnType],
    rx: Receiver<HistoryWork>,
    sink: HistoryWorkOutcomeSink,
) {
    while let Ok(work) = rx.recv() {
        let subject = match &work {
            HistoryWork::Query { ticket, .. } => Ok(*ticket),
            HistoryWork::Keys { column } => Err(column.clone()),
        };
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            geode_core::panic::contained(|| match work {
                HistoryWork::Keys { column } => {
                    let values = query.keys(table, &column);
                    HistoryWorkOutcome::Keys { column, values }
                }
                HistoryWork::Query {
                    ticket,
                    mut request,
                    value_arity,
                } => {
                    if request.table.is_empty() {
                        request.table = table.to_string();
                    }
                    let fetched_at = chrono::Utc::now();
                    match query
                        .query(&request)
                        .and_then(|rows| checked(rows, key_types, value_arity, &request))
                    {
                        Ok((rows, dropped)) => {
                            if dropped > 0 {
                                tracing::warn!(
                                    target: "geode::ingest",
                                    "history query on {}: {dropped} non-finite value(s) dropped",
                                    request.table
                                );
                            }
                            HistoryWorkOutcome::Fetched {
                                ticket,
                                request,
                                rows,
                                dropped,
                                fetched_at,
                            }
                        }
                        Err(e) => HistoryWorkOutcome::Failed {
                            ticket,
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
                tracing::error!(target: "geode::ingest", "a history query panicked: {message}");
                match subject {
                    Ok(ticket) => sink(HistoryWorkOutcome::Failed {
                        ticket,
                        reason: format!("history query panicked: {message}"),
                    }),
                    Err(column) => sink(HistoryWorkOutcome::KeysPanicked {
                        column,
                        payload: message,
                    }),
                }
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::adapter::{AdapterError, HistoryQuery, HistoryRequest, HistoryRows};
    use chrono::{DateTime, Duration, NaiveTime, TimeZone, Utc};
    use geode_core::query::{KeySet, Span};
    use geode_core::schema::ColumnType;
    use std::sync::Mutex;
    use std::sync::mpsc::{Receiver, channel};

    const WAIT: std::time::Duration = std::time::Duration::from_secs(10);

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    /// The fake's key: symbol, tenor, delta.
    pub(crate) fn key_types() -> Vec<ColumnType> {
        vec![ColumnType::Utf8, ColumnType::Utf8, ColumnType::F64]
    }

    /// Answers up to `days` daily points at 21:00 UTC for every asked tuple,
    /// completed to a full key with tenor 3M and delta 0.5 (spelled
    /// `delta_text` when set), two value columns (vol = 15 + i, forward =
    /// 7650). A `Between` starts at its `from` day and keeps only points in
    /// `[from, to)`; `Latest` and `At` start at 2026-01-05, and `At(t)` keeps
    /// only points at or before `t`. Day `nan_day` (if any) has a NaN vol.
    /// A tuple starting with "broken" fails; with `fail_once`, only the first
    /// time that key set is asked for. `stray` adds one row under a key
    /// nobody asked for. `keys` answers `self.keys` for any column. The call
    /// count lives in the shared `calls` log, so a copy handed to a worker
    /// still reports. With `hold`, each query is logged and then waits until
    /// the hold is released (or a bounded wait passes, so a test that never
    /// releases still ends).
    #[derive(Default)]
    pub(crate) struct FakeHistory {
        pub calls: Arc<Mutex<Vec<HistoryRequest>>>,
        pub days: usize,
        pub keys: Option<Vec<String>>,
        pub fail_once: bool,
        pub nan_day: Option<usize>,
        pub delta_text: Option<String>,
        pub stray: Option<Vec<String>>,
        pub hold: Option<Arc<Hold>>,
    }

    /// A gate a held `FakeHistory` waits on: `true` once released.
    #[derive(Default)]
    pub(crate) struct Hold {
        released: Mutex<bool>,
        cvar: std::sync::Condvar,
    }

    pub(crate) struct ReleaseOnDrop(Arc<Hold>);

    impl Drop for ReleaseOnDrop {
        fn drop(&mut self) {
            self.0.set(true);
        }
    }

    impl Hold {
        pub(crate) fn set(&self, released: bool) {
            *self.released.lock().unwrap_or_else(|e| e.into_inner()) = released;
            self.cvar.notify_all();
        }

        /// Releases the hold when dropped. Bind it after the worker or
        /// service so it drops first, on a failing assertion's unwind too:
        /// a worker queued behind a hold that is never released would make
        /// the drop's join wait out every queued query's bounded wait.
        pub(crate) fn release_on_drop(self: &Arc<Self>) -> ReleaseOnDrop {
            ReleaseOnDrop(Arc::clone(self))
        }

        pub(crate) fn wait(&self) {
            let released = self.released.lock().unwrap_or_else(|e| e.into_inner());
            let _ =
                self.cvar
                    .wait_timeout_while(released, std::time::Duration::from_secs(30), |r| !*r);
        }
    }

    impl HistoryQuery for FakeHistory {
        fn query(&mut self, req: &HistoryRequest) -> Result<HistoryRows, AdapterError> {
            let broken = req
                .keys
                .tuples()
                .iter()
                .any(|t| t.first().is_some_and(|s| s == "broken"));
            let asked_before = {
                let mut calls = self.calls.lock().unwrap_or_else(|e| e.into_inner());
                let asked_before = calls.iter().filter(|c| c.keys == req.keys).count();
                calls.push(req.clone());
                asked_before
            };
            if let Some(hold) = &self.hold {
                hold.wait();
            }
            if broken && !(self.fail_once && asked_before > 0) {
                return Err(AdapterError {
                    message: "no such symbol".into(),
                });
            }
            let start_day = match req.span {
                Span::Between { from, .. } => from.date_naive(),
                Span::Latest | Span::At(_) => ts("2026-01-05T00:00:00Z").date_naive(),
            };
            let at_21 = NaiveTime::from_hms_opt(21, 0, 0).unwrap();
            let points: Vec<(usize, DateTime<Utc>)> = (0..self.days)
                .map(|i| {
                    let day = start_day + Duration::days(i as i64);
                    (i, Utc.from_utc_datetime(&day.and_time(at_21)))
                })
                .filter(|(_, t)| match req.span {
                    Span::Between { from, to, .. } => from <= *t && *t < to,
                    Span::At(at) => *t <= at,
                    Span::Latest => true,
                })
                .collect();
            let delta = self.delta_text.clone().unwrap_or_else(|| "0.5".into());
            let defaults = [String::new(), "3M".to_string(), delta];
            let mut tuples: Vec<Vec<String>> = req
                .keys
                .tuples()
                .iter()
                .map(|t| {
                    let mut full = t.clone();
                    full.extend(defaults[full.len().min(3)..].iter().cloned());
                    full
                })
                .collect();
            if let Some(stray) = &self.stray {
                tuples.push(stray.clone());
            }
            let mut rows = HistoryRows {
                ts: Vec::new(),
                keys: vec![Vec::new(); 3],
                values: vec![Vec::new(); 2],
            };
            for tuple in &tuples {
                for (i, t) in &points {
                    rows.ts.push(*t);
                    for (c, v) in tuple.iter().enumerate() {
                        rows.keys[c].push(v.clone());
                    }
                    let vol = if self.nan_day == Some(*i) {
                        f64::NAN
                    } else {
                        15.0 + *i as f64
                    };
                    rows.values[0].push(vol);
                    rows.values[1].push(7650.0);
                }
            }
            Ok(rows)
        }

        fn keys(&mut self, _table: &str, _column: &str) -> Option<Vec<String>> {
            self.keys.clone()
        }
    }

    fn spawn(query: Box<dyn HistoryQuery>) -> (HistoryWorker, Receiver<HistoryWorkOutcome>) {
        let (tx, rx) = channel();
        let sink: HistoryWorkOutcomeSink = Arc::new(move |o| {
            let _ = tx.send(o);
        });
        let w = HistoryWorker::spawn(
            "demo_vol",
            "vol_history",
            key_types(),
            query,
            sink,
            crate::supervise::unwatched(),
        )
        .unwrap();
        (w, rx)
    }

    fn between(keys: KeySet) -> HistoryRequest {
        HistoryRequest {
            table: String::new(),
            keys,
            span: Span::Between {
                from: ts("2026-01-05T00:00:00Z"),
                to: ts("2026-01-12T00:00:00Z"),
                step: None,
            },
        }
    }

    fn spx() -> KeySet {
        KeySet::Prefix(vec![vec!["SPX".into()]])
    }

    #[test]
    fn a_query_yields_fetched_rows_with_non_finite_values_dropped() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let (mut w, rx) = spawn(Box::new(FakeHistory {
            calls: calls.clone(),
            days: 3,
            nan_day: Some(1),
            ..Default::default()
        }));
        assert!(w.request(HistoryWork::Query {
            ticket: Ticket(7),
            request: between(spx()),
            value_arity: 2,
        }));
        match rx.recv_timeout(WAIT).unwrap() {
            HistoryWorkOutcome::Fetched {
                ticket,
                request,
                rows,
                dropped,
                ..
            } => {
                assert_eq!(ticket, Ticket(7));
                assert_eq!(rows.len(), 2);
                assert_eq!(dropped, 1);
                assert_eq!(rows.values[0], vec![15.0, 17.0]);
                assert_eq!(
                    request.table, "vol_history",
                    "an empty table is filled with the source's table"
                );
            }
            other => panic!("{other:?}"),
        }
        {
            let calls = calls.lock().unwrap();
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].table, "vol_history", "the adapter sees the table");
        }
        w.shutdown();
    }

    #[test]
    fn a_key_spelled_differently_by_the_source_is_normalized_before_validation() {
        let (mut w, rx) = spawn(Box::new(FakeHistory {
            days: 1,
            delta_text: Some("0.50".into()),
            ..Default::default()
        }));
        let exact = KeySet::Exact(vec![vec!["SPX".into(), "3M".into(), "0.5".into()]]);
        assert!(w.request(HistoryWork::Query {
            ticket: Ticket(1),
            request: between(exact),
            value_arity: 2,
        }));
        match rx.recv_timeout(WAIT).unwrap() {
            HistoryWorkOutcome::Fetched { ticket, rows, .. } => {
                assert_eq!(ticket, Ticket(1));
                assert_eq!(rows.len(), 1);
                assert_eq!(rows.key_at(0), vec!["SPX", "3M", "0.5"]);
            }
            other => panic!("{other:?}"),
        }
        w.shutdown();
    }

    /// Answers any ask with one row keyed `SPX / 3M / 0.50`.
    struct SpellsDeltaLong;
    impl HistoryQuery for SpellsDeltaLong {
        fn query(&mut self, _: &HistoryRequest) -> Result<HistoryRows, AdapterError> {
            Ok(HistoryRows {
                ts: vec![ts("2026-01-05T21:00:00Z")],
                keys: vec![vec!["SPX".into()], vec!["3M".into()], vec!["0.50".into()]],
                values: vec![vec![15.0], vec![7650.0]],
            })
        }
        fn keys(&mut self, _: &str, _: &str) -> Option<Vec<String>> {
            None
        }
    }

    #[test]
    fn an_exact_ask_answered_in_another_spelling_is_accepted() {
        let (mut w, rx) = spawn(Box::new(SpellsDeltaLong));
        let exact = KeySet::Exact(vec![vec!["SPX".into(), "3M".into(), "0.5".into()]]);
        assert!(w.request(HistoryWork::Query {
            ticket: Ticket(8),
            request: between(exact),
            value_arity: 2,
        }));
        match rx.recv_timeout(WAIT).unwrap() {
            HistoryWorkOutcome::Fetched { ticket, rows, .. } => {
                assert_eq!(ticket, Ticket(8));
                assert_eq!(rows.key_at(0), vec!["SPX", "3M", "0.5"]);
            }
            other => panic!("`0.50` answering `0.5` is one key: {other:?}"),
        }
        w.shutdown();
    }

    #[test]
    fn an_unparsable_key_fails_the_ticket() {
        let (mut w, rx) = spawn(Box::new(FakeHistory {
            days: 1,
            delta_text: Some("half".into()),
            ..Default::default()
        }));
        assert!(w.request(HistoryWork::Query {
            ticket: Ticket(2),
            request: between(spx()),
            value_arity: 2,
        }));
        match rx.recv_timeout(WAIT).unwrap() {
            HistoryWorkOutcome::Failed { ticket, reason } => {
                assert_eq!(ticket, Ticket(2));
                assert!(reason.contains("not an f64 key"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
        w.shutdown();
    }

    #[test]
    fn an_adapter_error_is_a_failed_outcome_for_the_ticket() {
        let (mut w, rx) = spawn(Box::new(FakeHistory {
            days: 1,
            ..Default::default()
        }));
        assert!(w.request(HistoryWork::Query {
            ticket: Ticket(3),
            request: between(KeySet::Prefix(vec![vec!["broken".into()]])),
            value_arity: 2,
        }));
        match rx.recv_timeout(WAIT).unwrap() {
            HistoryWorkOutcome::Failed { ticket, reason } => {
                assert_eq!(ticket, Ticket(3));
                assert_eq!(reason, "no such symbol");
            }
            other => panic!("{other:?}"),
        }
        w.shutdown();
    }

    #[test]
    fn rows_for_a_key_outside_the_request_fail_the_ticket() {
        let (mut w, rx) = spawn(Box::new(FakeHistory {
            days: 1,
            stray: Some(vec!["NDX".into(), "3M".into(), "0.5".into()]),
            ..Default::default()
        }));
        assert!(w.request(HistoryWork::Query {
            ticket: Ticket(4),
            request: between(spx()),
            value_arity: 2,
        }));
        match rx.recv_timeout(WAIT).unwrap() {
            HistoryWorkOutcome::Failed { ticket, reason } => {
                assert_eq!(ticket, Ticket(4));
                assert!(reason.contains("not asked for"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
        w.shutdown();
    }

    #[test]
    fn keys_are_answered_and_shutdown_refuses_later_work() {
        let (mut w, rx) = spawn(Box::new(FakeHistory {
            keys: Some(vec!["1M".into(), "3M".into()]),
            ..Default::default()
        }));
        assert!(w.request(HistoryWork::Keys {
            column: "tenor".into()
        }));
        match rx.recv_timeout(WAIT).unwrap() {
            HistoryWorkOutcome::Keys { column, values } => {
                assert_eq!(column, "tenor");
                assert_eq!(values, Some(vec!["1M".to_string(), "3M".to_string()]));
            }
            other => panic!("{other:?}"),
        }
        w.shutdown();
        assert!(
            !w.request(HistoryWork::Keys {
                column: "tenor".into()
            }),
            "a stopped worker refuses"
        );
    }

    #[test]
    fn behind_a_held_query_the_bound_is_accepted_and_the_next_refused_without_blocking() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let hold = Arc::new(Hold::default());
        let (mut w, rx) = spawn(Box::new(FakeHistory {
            calls: calls.clone(),
            days: 1,
            hold: Some(hold.clone()),
            ..Default::default()
        }));
        let _release = hold.release_on_drop();
        let query = |n: u64| HistoryWork::Query {
            ticket: Ticket(n),
            request: between(spx()),
            value_arity: 2,
        };
        assert!(w.request(query(0)));
        // The held query has left the queue once the fake logged it.
        let deadline = std::time::Instant::now() + WAIT;
        while calls.lock().unwrap().is_empty() {
            assert!(
                std::time::Instant::now() < deadline,
                "the worker never took the query"
            );
            std::thread::yield_now();
        }
        for n in 1..=HISTORY_BOUND as u64 {
            assert!(w.request(query(n)), "queued item {n} of {HISTORY_BOUND}");
        }
        let started = std::time::Instant::now();
        assert!(!w.request(query(99)), "one past the bound is refused");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "without blocking"
        );
        hold.set(true);
        for _ in 0..=HISTORY_BOUND {
            assert!(matches!(
                rx.recv_timeout(WAIT).unwrap(),
                HistoryWorkOutcome::Fetched { .. }
            ));
        }
        w.shutdown();
    }

    struct PanickingHistory;
    impl HistoryQuery for PanickingHistory {
        fn query(&mut self, _: &HistoryRequest) -> Result<HistoryRows, AdapterError> {
            panic!("the history client fell over")
        }
        fn keys(&mut self, _: &str, column: &str) -> Option<Vec<String>> {
            if column == "boom" {
                panic!("the listing fell over")
            }
            Some(vec!["3M".into()])
        }
    }

    #[test]
    fn a_panicking_query_fails_its_ticket_and_the_worker_lives() {
        let (mut w, rx) = spawn(Box::new(PanickingHistory));
        assert!(w.request(HistoryWork::Query {
            ticket: Ticket(5),
            request: between(spx()),
            value_arity: 2,
        }));
        match rx.recv_timeout(WAIT).unwrap() {
            HistoryWorkOutcome::Failed { ticket, reason } => {
                assert_eq!(ticket, Ticket(5));
                assert!(reason.contains("history query panicked"), "{reason}");
                assert!(reason.contains("the history client fell over"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
        assert!(w.request(HistoryWork::Keys {
            column: "tenor".into()
        }));
        match rx.recv_timeout(WAIT).unwrap() {
            HistoryWorkOutcome::Keys { values, .. } => {
                assert_eq!(values, Some(vec!["3M".to_string()]))
            }
            other => panic!("{other:?}"),
        }
        w.shutdown();
    }

    #[test]
    fn a_panicking_key_listing_is_an_outcome_for_its_column() {
        let (mut w, rx) = spawn(Box::new(PanickingHistory));
        assert!(w.request(HistoryWork::Keys {
            column: "boom".into()
        }));
        match rx.recv_timeout(WAIT).unwrap() {
            HistoryWorkOutcome::KeysPanicked { column, payload } => {
                assert_eq!(column, "boom");
                assert!(payload.contains("the listing fell over"), "{payload}");
            }
            other => panic!("{other:?}"),
        }
        w.shutdown();
    }

    #[test]
    fn a_history_worker_that_dies_is_declared() {
        let (stop, stops) = crate::supervise::tests_support::recording();
        let sink: HistoryWorkOutcomeSink = Arc::new(|_| panic!("the history sink fell over"));
        let mut w = HistoryWorker::spawn(
            "demo_vol",
            "vol_history",
            key_types(),
            Box::new(FakeHistory::default()),
            sink,
            stop,
        )
        .unwrap();
        assert!(w.request(HistoryWork::Keys {
            column: "tenor".into()
        }));
        let (thread, reason) = crate::supervise::tests_support::next_stop(&stops);
        assert_eq!(thread, "geode-history-demo_vol");
        assert!(reason.contains("the history sink fell over"), "{reason}");
        w.shutdown();
    }
}
