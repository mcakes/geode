//! The history request's bookkeeping between the service, the source's
//! history worker, the ingest runner and the query pool.
//!
//! A request (a history read or a board) either reads the cache at once or
//! opens a ticket for the adapter calls its coverage lacks; each call's
//! append (or failure) finishes one part of the ticket, and the last one
//! answers it. A history request answers any failure as its error; a board
//! answers a failure of its surface as its error and carries any other
//! dataset's failure in `dataset_errors`. Otherwise the read goes to the
//! pool under the request's tag: a history read under the requester's key,
//! a board read under `board_pool_key` of it.
//!
//! Every request is recorded as the newest for its key, whatever path it
//! takes. A ticket whose request has been superseded answers nothing when
//! it finishes: submitting its read would replace the newer request's read
//! at the pool, which coalesces latest-submitted-wins, and an answer for an
//! older tag would land after the newer one. The newest check and the
//! pool submission happen under one lock for the same reason.

use crate::adapter::{HistoryRequest, HistoryRows};
use crate::ingest::history::Ticket;
use crate::query::history::HistoryRead;
use crate::query::pool::{QueryPool, QueryRequest, RequestKind, ViewId, Work};
use crate::query::volboard::{BoardPlan, BoardRead, board_pool_key};
use crate::service::{DataEvent, EventSink};
use crate::store::StoreError;
use crate::store::history::{CoverSpan, Covered, covers_instant, missing_by_key, uncovered_at};
use chrono::{DateTime, Utc};
use geode_core::query::{HistoryOutcome, HistoryParams, KeySet, QueryKey, Span, normalize_key};
use geode_core::schema::{DatasetSpec, SchemaSpec};
use geode_core::snapshot::Provenance;
use geode_core::volboard::VolBoardOutcome;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

/// What a ticket answers when its calls finish: a history read, or a
/// board read over the datasets whose calls succeeded.
///
/// `Clone`: `refuse_ticket` answers from a clone of the pending answer.
#[derive(Clone)]
pub(crate) enum Answer {
    History(HistoryParams),
    Board(Box<BoardPlan>),
}

impl Answer {
    /// The requester's key: what the answer is addressed to, and what
    /// supersession is keyed by.
    pub(crate) fn key(&self) -> QueryKey {
        match self {
            Answer::History(p) => p.key,
            Answer::Board(b) => b.params.key,
        }
    }
}

pub(crate) struct HistoryDispatch {
    state: Mutex<State>,
    pool: Arc<QueryPool>,
    schema: Arc<SchemaSpec>,
    sink: EventSink,
}

#[derive(Default)]
struct State {
    /// The last request number handed out; a ticket is its request's
    /// number.
    next: u64,
    /// The newest request number per requester key.
    newest: HashMap<QueryKey, u64>,
    pending: HashMap<Ticket, Pending>,
}

struct Pending {
    answer: Answer,
    /// Adapter calls (each with its append) not yet finished.
    outstanding: usize,
    /// `(dataset, reason)` per failed call, repeats dropped.
    failures: Vec<(String, String)>,
    /// Already answered (a full queue refused it): the calls that were
    /// queued still append, but their finish answers nothing more.
    answered: bool,
}

impl HistoryDispatch {
    pub(crate) fn new(pool: Arc<QueryPool>, schema: Arc<SchemaSpec>, sink: EventSink) -> Self {
        HistoryDispatch {
            state: Mutex::new(State::default()),
            pool,
            schema,
            sink,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn supersede(state: &mut State, key: QueryKey) -> u64 {
        state.next += 1;
        state.newest.insert(key, state.next);
        state.next
    }

    /// Answer a request refused before any work: the newest for its key.
    pub(crate) fn refuse(&self, answer: &Answer, reason: String) {
        let mut state = self.lock();
        Self::supersede(&mut state, answer.key());
        self.answer_err(answer, reason);
    }

    /// Read now, no fetch needed.
    pub(crate) fn read_now(&self, answer: Answer) {
        let mut state = self.lock();
        Self::supersede(&mut state, answer.key());
        self.submit(answer);
    }

    /// A requester's cancel: nothing in flight for `key` answers any more.
    pub(crate) fn cancel(&self, key: QueryKey) {
        let mut state = self.lock();
        if state.newest.contains_key(&key) {
            Self::supersede(&mut state, key);
        }
    }

    /// Open a ticket for `calls` adapter calls (at least one: a ticket with
    /// none would never be finished, and so never answered).
    ///
    /// The newest request is tracked per requester key whatever the answer
    /// kind: a board and a history request under one key supersede each
    /// other, so a requester asking both must use two keys.
    pub(crate) fn open(&self, answer: Answer, calls: usize) -> Ticket {
        debug_assert!(calls > 0, "a ticket waits on at least one call");
        let mut state = self.lock();
        let ticket = Ticket(Self::supersede(&mut state, answer.key()));
        state.pending.insert(
            ticket,
            Pending {
                answer,
                outstanding: calls,
                failures: Vec::new(),
                answered: false,
            },
        );
        ticket
    }

    /// Answer `ticket` with `reason` now: `unqueued` of its calls were never
    /// made and so will never finish. The calls already queued still finish
    /// (filling the cache) but answer nothing more. Waiting for them instead
    /// would leave the request unanswered while the queue stays full.
    pub(crate) fn refuse_ticket(&self, ticket: Ticket, unqueued: usize, reason: String) {
        let mut state = self.lock();
        let Some(p) = state.pending.get_mut(&ticket) else {
            return;
        };
        p.outstanding = p.outstanding.saturating_sub(unqueued);
        let first = !p.answered;
        p.answered = true;
        let answer = p.answer.clone();
        if p.outstanding == 0 {
            state.pending.remove(&ticket);
        }
        if first && state.newest.get(&answer.key()) == Some(&ticket.0) {
            self.answer_err(&answer, reason);
        }
    }

    /// One adapter call (with its append) for `dataset` finished. On the
    /// last one a history answer refuses with every failure joined, else
    /// reads; a board refuses with its surface's failures, else blanks and
    /// names each other failed dataset and reads. A read is submitted to
    /// the pool under the request's tag. A superseded or already answered
    /// ticket answers nothing.
    pub(crate) fn finish(&self, ticket: Ticket, dataset: &str, result: Result<(), String>) {
        let mut state = self.lock();
        let Some(p) = state.pending.get_mut(&ticket) else {
            return;
        };
        p.outstanding = p.outstanding.saturating_sub(1);
        if let Err(reason) = result {
            let entry = (dataset.to_string(), reason);
            if !p.failures.contains(&entry) {
                p.failures.push(entry);
            }
        }
        if p.outstanding > 0 {
            return;
        }
        let Some(p) = state.pending.remove(&ticket) else {
            return;
        };
        if p.answered || state.newest.get(&p.answer.key()) != Some(&ticket.0) {
            return;
        }
        match p.answer {
            Answer::History(params) => {
                if p.failures.is_empty() {
                    self.submit(Answer::History(params));
                } else {
                    let reasons: Vec<String> = p.failures.into_iter().map(|(_, r)| r).collect();
                    self.answer_err(&Answer::History(params), reasons.join("; "));
                }
            }
            Answer::Board(mut plan) => {
                // The surface is the board: its failure is the request's.
                // Any other dataset's failure blanks its cells and names
                // itself; the board is otherwise current.
                let (surface, others): (Vec<_>, Vec<_>) = p
                    .failures
                    .into_iter()
                    .partition(|(ds, _)| *ds == plan.params.surface);
                if !surface.is_empty() {
                    let reasons: Vec<String> = surface.into_iter().map(|(_, r)| r).collect();
                    self.answer_err(&Answer::Board(plan), reasons.join("; "));
                    return;
                }
                for (ds, reason) in others {
                    match plan.dataset_errors.iter_mut().find(|(d, _)| *d == ds) {
                        Some((_, r)) => {
                            r.push_str("; ");
                            r.push_str(&reason);
                        }
                        None => plan.dataset_errors.push((ds, reason)),
                    }
                }
                self.submit(Answer::Board(plan));
            }
        }
    }

    /// Called under the state lock. Anything for this request's pool key
    /// still in the pool belongs to an older request; cancelling it keeps
    /// its answer from arriving after this one.
    fn answer_err(&self, answer: &Answer, reason: String) {
        match answer {
            Answer::History(params) => {
                self.pool.cancel(params.key);
                let _ = (self.sink)(DataEvent::History(HistoryOutcome {
                    key: params.key,
                    tag: params.tag,
                    submitted: params.submitted,
                    dataset: params.dataset.clone(),
                    span: params.span,
                    result: Err(reason),
                }));
            }
            Answer::Board(plan) => {
                self.pool.cancel(board_pool_key(plan.params.key));
                let _ = (self.sink)(DataEvent::VolBoard(VolBoardOutcome {
                    key: plan.params.key,
                    tag: plan.params.tag,
                    submitted: plan.params.submitted,
                    result: Err(reason),
                }));
            }
        }
    }

    /// Called under the state lock. A history read runs under the
    /// requester's key; a board read under `board_pool_key` of it, so the
    /// requester's own view queries neither replace nor cancel it.
    fn submit(&self, answer: Answer) {
        match answer {
            Answer::History(params) => {
                let kind = RequestKind::History {
                    dataset: params.dataset.clone(),
                    span: params.span,
                };
                self.pool.submit(QueryRequest {
                    key: params.key,
                    tag: params.tag,
                    submitted: params.submitted,
                    view: ViewId(format!("history:{}", params.dataset)),
                    grouping: Vec::new(),
                    provenance: Provenance::default(),
                    work: Work::History(Box::new(HistoryRead::new(
                        Arc::clone(&self.schema),
                        params,
                    ))),
                    kind,
                });
            }
            Answer::Board(plan) => {
                let (key, tag, submitted) =
                    (plan.params.key, plan.params.tag, plan.params.submitted);
                let view = ViewId(format!("volboard:{}", plan.params.surface));
                self.pool.submit(QueryRequest {
                    key: board_pool_key(key),
                    tag,
                    submitted,
                    view,
                    grouping: Vec::new(),
                    provenance: Provenance::default(),
                    kind: RequestKind::VolBoard { requester: key },
                    work: Work::VolBoard(Box::new(BoardRead::new(Arc::clone(&self.schema), *plan))),
                });
            }
        }
    }
}

/// The asked keys in one spelling per value (each column's declared type),
/// repeats removed in order, so coverage, the adapter call and the read all
/// see `0.5` whether `0.5` or `0.50` was asked. Assumes a validated set.
pub(crate) fn normalize_keys(ds: &DatasetSpec, keys: &KeySet) -> Result<KeySet, String> {
    let columns = ds.history_key();
    let mut out: Vec<Vec<String>> = Vec::with_capacity(keys.tuples().len());
    for tuple in keys.tuples() {
        let normalized = tuple
            .iter()
            .zip(&columns)
            .map(|(v, c)| {
                normalize_key(v, c.ty).map_err(|e| format!("key column '{}': {e}", c.name))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if !out.contains(&normalized) {
            out.push(normalized);
        }
    }
    Ok(same_kind(keys, out))
}

fn same_kind(like: &KeySet, tuples: Vec<Vec<String>>) -> KeySet {
    match like {
        KeySet::Prefix(_) => KeySet::Prefix(tuples),
        KeySet::Exact(_) => KeySet::Exact(tuples),
    }
}

/// The adapter calls a request needs before the cache can answer it; empty
/// means read now.
///
/// - `Latest` always asks the source: only it knows whether a newer
///   observation exists.
/// - `At(t)` reads the cache for every exact tuple it can answer
///   (`uncovered_at`) and asks the source for the rest in one call; a
///   prefix ask goes whole, since the cache cannot enumerate its members.
/// - `Between` is clipped to the dataset's history window first (retention
///   would delete what an older span fetched, and the next ask would fetch
///   it again forever; a wholly expired span asks nothing) and its end to
///   `now`, then one call per distinct gap for the tuples sharing that gap
///   list. The read still answers the span as asked.
pub(crate) fn plan_calls(
    conn: &duckdb::Connection,
    ds: &DatasetSpec,
    table: &str,
    keys: &KeySet,
    span: Span,
    now: DateTime<Utc>,
) -> Result<Vec<HistoryRequest>, StoreError> {
    let call = |keys: KeySet, span: Span| HistoryRequest {
        table: table.to_string(),
        keys,
        span,
    };
    match span {
        Span::Latest => Ok(vec![call(keys.clone(), span)]),
        Span::At(t) => match keys {
            // Each exact tuple the cache can answer is read from it; only
            // the rest go to the source. An absent tuple (a member the
            // surface lacks) therefore costs one small call per ask, not a
            // refetch of the whole set.
            KeySet::Exact(_) => {
                let uncovered = uncovered_at(conn, ds, keys.tuples(), t)?;
                if uncovered.is_empty() {
                    Ok(Vec::new())
                } else {
                    Ok(vec![call(KeySet::Exact(uncovered), span)])
                }
            }
            KeySet::Prefix(_) => {
                if covers_instant(conn, ds, keys.tuples(), t)? {
                    Ok(Vec::new())
                } else {
                    Ok(vec![call(keys.clone(), span)])
                }
            }
        },
        Span::Between { from, to, step } => {
            let from = match history_cutoff(ds, now) {
                Some(cutoff) => from.max(cutoff),
                None => from,
            };
            // Nothing after now can be covered, so the part of the span
            // past it would be a gap asked for on every request.
            let to = to.min(now);
            if from >= to {
                return Ok(Vec::new());
            }
            let mut by_gaps: BTreeMap<Vec<CoverSpan>, Vec<Vec<String>>> = BTreeMap::new();
            for (tuple, gaps) in missing_by_key(conn, ds, keys.tuples(), (from, to))? {
                if !gaps.is_empty() {
                    by_gaps.entry(gaps).or_default().push(tuple);
                }
            }
            Ok(by_gaps
                .into_iter()
                .flat_map(|(gaps, tuples)| {
                    gaps.into_iter().map(move |(from, to)| {
                        call(
                            same_kind(keys, tuples.clone()),
                            Span::Between { from, to, step },
                        )
                    })
                })
                .collect())
        }
    }
}

/// `now` less the dataset's history window, or `None` when it has none
/// (or the subtraction overflows: append still applies its retention).
fn history_cutoff(ds: &DatasetSpec, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let window = ds.series_retention?.history?;
    now.checked_sub_signed(chrono::Duration::from_std(window).ok()?)
}

/// What a source's answer to `request` establishes, per asked tuple:
///
/// - `Between` without a step: the call's whole span.
/// - `Between` with a step: nothing. The answer is thinned and must never
///   stand in for a full one later.
/// - `At(t)`: from the tuple's latest answered row to just past `t` for an
///   exact tuple with rows (the source asserts nothing lies between), else
///   just `t` itself.
/// - `Latest`: just the latest answered instant, for a tuple with rows.
///
/// Every span ends by `fetched_at` less [`crate::HISTORY_SETTLE`]: the
/// source could vouch for nothing after it was asked, and an observation
/// stamped shortly before the ask may not be published yet. Without the
/// clip a span reaching past now would mark the future covered and its
/// observations would never be fetched. A span the clip empties records
/// nothing.
pub(crate) fn covered_by(
    request: &HistoryRequest,
    rows: &HistoryRows,
    fetched_at: DateTime<Utc>,
) -> Vec<Covered> {
    let tick = chrono::Duration::microseconds(1);
    let bound = chrono::Duration::from_std(crate::HISTORY_SETTLE)
        .ok()
        .and_then(|settle| fetched_at.checked_sub_signed(settle))
        .unwrap_or(fetched_at);
    let latest_of = |tuple: &[String]| {
        (0..rows.ts.len())
            .filter(|&i| {
                tuple
                    .iter()
                    .zip(&rows.keys)
                    .all(|(v, column)| column.get(i) == Some(v))
            })
            .map(|i| rows.ts[i])
            .max()
    };
    let tuples = request.keys.tuples();
    let clipped = |asked: &Vec<String>, start: DateTime<Utc>, end: DateTime<Utc>| {
        let end = end.min(bound);
        (start < end).then(|| Covered {
            asked: asked.clone(),
            span: (start, end),
        })
    };
    let instant = |asked: &Vec<String>, at: DateTime<Utc>, start: DateTime<Utc>| {
        at.checked_add_signed(tick)
            .and_then(|end| clipped(asked, start, end))
    };
    match request.span {
        Span::Between {
            from,
            to,
            step: None,
        } => tuples
            .iter()
            .filter_map(|asked| clipped(asked, from, to))
            .collect(),
        Span::Between { step: Some(_), .. } => Vec::new(),
        Span::At(t) => tuples
            .iter()
            .filter_map(|asked| {
                let start = match request.keys {
                    KeySet::Exact(_) => latest_of(asked).map_or(t, |m| m.min(t)),
                    KeySet::Prefix(_) => t,
                };
                instant(asked, t, start)
            })
            .collect(),
        Span::Latest => tuples
            .iter()
            .filter_map(|asked| {
                let m = latest_of(asked)?;
                instant(asked, m, m)
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::ddl::tests_support::{history_dataset, ts};

    fn rows(ts_: &[&str], keys: &[&[&str]]) -> HistoryRows {
        HistoryRows {
            ts: ts_.iter().map(|s| ts(s)).collect(),
            keys: keys
                .iter()
                .map(|c| c.iter().map(|s| s.to_string()).collect())
                .collect(),
            values: vec![vec![1.0; ts_.len()]],
        }
    }

    fn exact(t: &[&str]) -> Vec<String> {
        t.iter().map(|s| s.to_string()).collect()
    }

    /// A fetch time long after every span below, so no clip applies.
    fn settled() -> DateTime<Utc> {
        ts("2027-01-01T00:00:00Z")
    }

    /// `history_dataset()` with SPX 3M 0.5 rows on Jan 5–7 and both SPX 3M
    /// 0.5 and NDX 3M 0.5 covered over `[Jan 5, Jan 10)`: NDX has no row.
    fn covered_store() -> (tempfile::TempDir, crate::store::Store, DatasetSpec) {
        use crate::store::history::{HistoryAppendRequest, append_history};
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("geode.duckdb")).unwrap();
        let ds = history_dataset();
        store.apply_schema(&ds).unwrap();
        crate::store::catalog::Catalog::new(store.writer())
            .ensure_tables()
            .unwrap();
        let days = [
            "2026-01-05T21:00:00Z",
            "2026-01-06T21:00:00Z",
            "2026-01-07T21:00:00Z",
        ];
        let rows = HistoryRows {
            ts: days.iter().map(|d| ts(d)).collect(),
            keys: vec![
                vec!["SPX".into(); 3],
                vec!["3M".into(); 3],
                vec!["0.5".into(); 3],
            ],
            values: vec![vec![15.0; 3], vec![7650.0; 3]],
        };
        let week = (ts("2026-01-05T00:00:00Z"), ts("2026-01-10T00:00:00Z"));
        append_history(
            &store,
            &HistoryAppendRequest {
                dataset: &ds,
                source: "hanweck",
                rows: &rows,
                covered: vec![
                    Covered {
                        asked: exact(&["SPX", "3M", "0.5"]),
                        span: week,
                    },
                    Covered {
                        asked: exact(&["NDX", "3M", "0.5"]),
                        span: week,
                    },
                ],
                received_at: ts("2026-01-10T09:00:00Z"),
            },
        )
        .unwrap();
        (dir, store, ds)
    }

    #[test]
    fn an_exact_at_asks_only_the_tuples_coverage_cannot_answer() {
        let (_d, store, ds) = covered_store();
        let spx = exact(&["SPX", "3M", "0.5"]);
        let ndx = exact(&["NDX", "3M", "0.5"]);
        let at = ts("2026-01-08T12:00:00Z");
        let plan = |keys: KeySet| {
            plan_calls(
                store.writer(),
                &ds,
                "surface",
                &keys,
                Span::At(at),
                settled(),
            )
            .unwrap()
        };
        let calls = plan(KeySet::Exact(vec![spx.clone(), ndx.clone()]));
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(
            calls[0].keys,
            KeySet::Exact(vec![ndx.clone()]),
            "SPX is read from the cache"
        );
        assert_eq!(calls[0].span, Span::At(at));
        assert_eq!(calls[0].table, "surface");
        assert!(plan(KeySet::Exact(vec![spx])).is_empty());
        // A prefix ask still goes whole, even where every member under it is held.
        let prefix = plan(KeySet::Prefix(vec![exact(&["SPX"])]));
        assert_eq!(prefix.len(), 1);
        assert_eq!(prefix[0].keys, KeySet::Prefix(vec![exact(&["SPX"])]));
    }

    #[test]
    fn coverage_ends_at_the_fetch_time_less_the_settle_margin() {
        let fetched_at = ts("2026-01-08T12:00:00Z");
        let bound = fetched_at - chrono::Duration::from_std(crate::HISTORY_SETTLE).unwrap();
        let r = rows(&["2026-01-07T21:00:00Z"], &[&["SPX"], &["3M"], &["0.5"]]);
        let spx = exact(&["SPX", "3M", "0.5"]);
        let ask = |keys: KeySet, span| HistoryRequest {
            table: String::new(),
            keys,
            span,
        };
        let from = ts("2026-01-05T00:00:00Z");
        let past_now = Span::Between {
            from,
            to: ts("2026-01-10T00:00:00Z"),
            step: None,
        };
        assert_eq!(
            covered_by(
                &ask(KeySet::Prefix(vec![exact(&["SPX"])]), past_now),
                &r,
                fetched_at
            ),
            vec![Covered {
                asked: exact(&["SPX"]),
                span: (from, bound)
            }]
        );
        let unsettled = Span::Between {
            from: bound,
            to: ts("2026-01-10T00:00:00Z"),
            step: None,
        };
        assert!(
            covered_by(
                &ask(KeySet::Prefix(vec![exact(&["SPX"])]), unsettled),
                &r,
                fetched_at
            )
            .is_empty(),
            "a span the clip empties records nothing"
        );
        let future = Span::At(ts("2026-01-09T00:00:00Z"));
        assert_eq!(
            covered_by(
                &ask(KeySet::Exact(vec![spx.clone()]), future),
                &r,
                fetched_at
            ),
            vec![Covered {
                asked: spx.clone(),
                span: (ts("2026-01-07T21:00:00Z"), bound)
            }],
            "an instant past the bound covers only up to it"
        );
        assert!(
            covered_by(
                &ask(KeySet::Prefix(vec![exact(&["SPX"])]), future),
                &r,
                fetched_at
            )
            .is_empty(),
            "a prefix point past the bound records nothing"
        );
    }

    #[test]
    fn a_stepped_between_records_no_coverage_and_a_full_one_records_its_span() {
        let (from, to) = (ts("2026-01-05T00:00:00Z"), ts("2026-01-10T00:00:00Z"));
        let r = rows(&["2026-01-05T21:00:00Z"], &[&["SPX"], &["3M"], &["0.5"]]);
        let ask = |step| HistoryRequest {
            table: "surface".into(),
            keys: KeySet::Prefix(vec![exact(&["SPX"])]),
            span: Span::Between { from, to, step },
        };
        assert_eq!(
            covered_by(&ask(None), &r, settled()),
            vec![Covered {
                asked: exact(&["SPX"]),
                span: (from, to)
            }]
        );
        assert!(
            covered_by(
                &ask(Some(std::time::Duration::from_secs(86_400))),
                &r,
                settled()
            )
            .is_empty(),
            "a thinned answer is not coverage"
        );
    }

    #[test]
    fn an_at_answer_covers_from_the_exact_tuples_last_row_and_latest_only_where_rows_exist() {
        let t = ts("2026-01-07T22:00:00Z");
        let tick = chrono::Duration::microseconds(1);
        let r = rows(
            &["2026-01-05T21:00:00Z", "2026-01-06T21:00:00Z"],
            &[&["SPX", "SPX"], &["3M", "3M"], &["0.5", "0.5"]],
        );
        let spx = exact(&["SPX", "3M", "0.5"]);
        let ndx = exact(&["NDX", "3M", "0.5"]);
        let at = HistoryRequest {
            table: String::new(),
            keys: KeySet::Exact(vec![spx.clone(), ndx.clone()]),
            span: Span::At(t),
        };
        assert_eq!(
            covered_by(&at, &r, settled()),
            vec![
                Covered {
                    asked: spx.clone(),
                    span: (ts("2026-01-06T21:00:00Z"), t + tick)
                },
                Covered {
                    asked: ndx.clone(),
                    span: (t, t + tick)
                },
            ]
        );
        let latest = HistoryRequest {
            span: Span::Latest,
            ..at
        };
        let last = ts("2026-01-06T21:00:00Z");
        assert_eq!(
            covered_by(&latest, &r, settled()),
            vec![Covered {
                asked: spx,
                span: (last, last + tick)
            }],
            "a tuple with no row records nothing"
        );
    }

    #[test]
    fn asked_keys_are_normalized_per_column_type_and_repeats_removed() {
        let ds = history_dataset();
        let keys = KeySet::Exact(vec![
            exact(&[" SPX", "3M", "0.50"]),
            exact(&["SPX", "3M", "0.5"]),
        ]);
        assert_eq!(
            normalize_keys(&ds, &keys).unwrap(),
            KeySet::Exact(vec![exact(&["SPX", "3M", "0.5"])])
        );
        let bad = KeySet::Exact(vec![exact(&["SPX", "3M", "half"])]);
        assert!(
            normalize_keys(&ds, &bad)
                .unwrap_err()
                .contains("not an f64 key")
        );
    }

    #[test]
    fn an_answer_names_its_requester() {
        let params = HistoryParams {
            key: QueryKey(5),
            tag: 2,
            submitted: std::time::Instant::now(),
            dataset: "vol_surface".into(),
            keys: KeySet::Prefix(vec![exact(&["SPX"])]),
            span: Span::Latest,
        };
        assert_eq!(Answer::History(params).key(), QueryKey(5));
        let plan = crate::query::volboard::BoardPlan {
            params: geode_core::volboard::VolBoardParams {
                key: QueryKey(6),
                tag: 3,
                submitted: std::time::Instant::now(),
                surface: "vol_surface".into(),
                realized: None,
                varswap: None,
                members: vec!["SPX".into()],
                measures: vec![geode_core::volboard::Measure::atfm("3M")],
                lookbacks: vec![],
                window: None,
                sparkline: None,
            },
            now: settled(),
            instants: vec![],
            clock: geode_core::clock::Clock::utc(),
            dataset_errors: vec![],
        };
        let board = Answer::Board(Box::new(plan));
        assert_eq!(board.key(), QueryKey(6));
    }
}
