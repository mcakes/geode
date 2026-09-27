# Data-Layer Containment and Liveness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** No data-layer panic or thread death ends silently: a panicking request is answered once with an error, a dying data thread is declared once and shown in the status bar, five contained panics that became "no data" are reported, and every refused submission says whether it was `Busy` or `Stopped`.

**Architecture:** `geode-data` gains one supervision helper (`supervise::spawn_supervised`) that every long-lived data thread is spawned through; it emits `DataEvent::ThreadStopped` when a body unwinds. The request loop (`serve`) runs each arm and the view-replacement step inside `catch_unwind` + `contained`, answering a panic through the arm's own answer door; `DataHandle` submissions return `Result<(), Refusal>`. `kind.write` moves from the request loop onto the target's egress worker. The shell's `Diagnostics` folds `ThreadStopped` and the handle's busy count into prepared status-bar segments; modules and the bridge word and retry by refusal kind.

**Tech Stack:** Rust 2024, std threads and `catch_unwind`, DuckDB (duckdb-rs 1.10505), GPUI + gpui-component, `scripts/mutation-check.sh`.

**Spec:** `docs/superpowers/specs/2026-09-27-geode-containment-and-liveness-design.md` (binding; this plan argues from it and from the code as read on 2026-09-27; see "Spec deviations").

## Global Constraints

- No raw colors or literal radii: theme tokens (`theme.danger`, `theme.warning`, `theme.status_bar`, `theme.radius_tokens().sm`) and the rem scale only.
- No state mutation, I/O or unbounded allocation in render. Segment text and tooltips are built when state changes (`Diagnostics::note_*`), never per frame.
- Displayed times go through `geode_core::clock::Clock`; never `chrono::Local`.
- The UI thread never waits on data work. Every `DataHandle` submission stays a non-blocking `try_send`.
- Comments state the local invariant and the failure it prevents. No task numbers, review ids, spec section numbers or dates in code comments.
- Update `docs/current/*` guides and crate READMEs in the same change (Task 8 does it before the branch merges).
- `geode-shell` and `geode-data` never depend on each other; the shell repeats the thread name `geode-data` rather than importing it.
- Every new library/binary target sets `bench = false` (no new targets are planned).
- Mutation harness rules: never run `--changed`; never an unfiltered mutation run or unfiltered `--build-check`; mode flags come first (`--build-check "<name>"`); probe by entry-name substring only; check for a running harness with `pgrep -f 'mutation-che[c]k'` before starting one; commit before any mutation run (the harness edits tracked files in place).
- Every new or re-aimed mutation entry is verified three ways before its task is done: `zsh scripts/mutation-check.sh --build-check "<name>"` reports it built and compiling; `zsh scripts/mutation-check.sh "<name>"` prints `caught    <name>` (not `caught*`, `SURVIVED`, `FILTER` or `BUILD`); and the replacement applied by hand makes the named test fail on an assertion or an `expect` message (then restore the original text exactly and confirm `git diff` shows nothing for that hunk).
- New mutation entries go in a new section `# ---- containment and liveness` placed immediately after the last `run_mutation` entry and before the line `if [[ -n "$changed_ref" ]]; then` in `scripts/mutation-check.sh`. Never read that file whole; locate entries with `grep -n`.
- Anchors are exact text including indentation. After `cargo fmt`, re-read each anchored line in the source and correct the entry to the formatted text; `zsh scripts/mutation-check.sh --anchors-only` must exit 0 at the end of every task.
- Gate at the end of every task: the task's focused tests, then `cargo test -p <crate>` for each touched crate, `cargo clippy -p <crate> --all-targets -- -D warnings` for each touched crate, `cargo fmt --check`, `zsh scripts/mutation-check.sh --anchors-only`. Tasks touching `geode-shell` also run `cargo check -p geode-shell --features test-support --all-targets`.
- Commit trailer on every commit: `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Wording (final, subject to the display check): `Refusal::Busy` displays as `the data service is busy`; `Refusal::Stopped` as `the data service has stopped`. A panicking request answers `<kind> request panicked: <payload>`.

## Review Focus

1. Two different data threads stopping between two UI drains: both must reach the status bar (the bridge mailbox coalesces by key). Pinned in Task 1 (`two_threads_stopping_between_drains_are_both_delivered`).
2. A submission made while the dying request loop is still joining its workers (seconds, when a pricer line or a load is running) must be refused `Stopped`, not admitted to a queue nothing will read. Pinned in Task 4 (`a_submission_while_the_dying_loop_joins_its_workers_is_refused_stopped`).
3. A clean quit must never paint a stopped segment or mark the handle stopped. Pinned in Task 4 (`a_clean_shutdown_declares_nothing`).
4. A pricer tile on a stopped service with periodic refresh on must not start a backoff loop and must keep saying "stopped" across refresh ticks. Pinned in Task 7 (`a_stopped_service_stops_the_pricing_backoff`).
5. The same thread reported stopped twice (redelivery) must be recorded once. Pinned in Task 6 (`a_thread_reported_twice_is_recorded_once`).

## Spec deviations

Each is written to the code; evidence in brackets.

1. **The shared payload helper** is `crate::ingest::runner::panic_payload_message` [runner.rs:964, used by fetch.rs, egress.rs, subscribe.rs]. `query/pool.rs::panic_message` and `pricing/worker.rs::panic_message` are older private copies with different output; they are left alone (out of scope). All new code uses the runner helper.
2. **Worker spawns gain a `stop: EventSink` parameter.** The pool, pricing, ingest, discovery, fetch and subscription workers are spawned with typed sinks (`ResultSink`, `PriceSink`, `IngestSink`, `SchedulerSink`, `FetchOutcomeSink`, `LoadReportSink`), not an `EventSink` [service.rs:713-1325]. `DataService::open` holds the `EventSink` and passes it. Channel-delivering test constructors (`QueryPool::spawn`, `IngestRunner::spawn_channel`) pass `supervise::unwatched()`. `SubscriptionWorker::spawn` reaches eight parameters and carries `#[allow(clippy::too_many_arguments)]`, as `shell::status::status_bar` already does.
3. **Not supervised:** the channel adapter's dispatcher (`geode-channel-<name>`, adapter/channel.rs:80) and the demo bus thread (geode-app demo_bus.rs:95). Both are transport-tier threads created by `geode-app` before any `EventSink` exists; they stand in for a vendor client's own threads, which Geode will not own either.
4. **Open failure emits `ThreadStopped` itself.** It returns normally, so `spawn_supervised` emits nothing, yet §6 shows the request-loop segment for an open failure. `serve` emits `ThreadStopped { thread: "geode-data", reason: "data service failed to open: <e>" }` beside the existing Diagnostic.
5. **Where `stopped` is set.** A drop guard declared after `service` in `serve` sets it when the loop unwinds, before the service's workers are joined; the open-failure path sets it before its Diagnostic. The supervisor's sink does not set it: by the time the helper emits, the receiver has already dropped and `Disconnected` already says `Stopped`, so only the guard closes the window while a dying loop joins its workers (Review Focus 2).
6. **`dropped` counts `Busy` only.** §6 backs "`N refused`" (submissions refused because the loop was busy) with `dropped_requests()`. A `Stopped` refusal and a post-shutdown refusal no longer count. Two existing tests change their counts (`a_gone_service_thread_refuses_every_request`, `the_real_service_answers_through_the_sink_and_reports_open_failures`).
7. **`cancel` keeps `-> bool`** (true = admitted). No caller acts on a refused cancel (a stale answer is dropped by its receiver's tag check anyway), five mutation entries anchor on the bare statement `self.data.cancel(QueryKey(self.id.0));`, and a `Result` would force `let _ =` onto every one. Its refusals follow the same `Busy`/`Stopped` counting rule.
8. **`replace_views` can only refuse `Stopped`:** a full queue keeps the latest views in the mailbox and answers `Ok` [handle.rs:230-253]. The §7 "`Busy` → warning Diagnostic" row is unreachable; the bridge still matches it.
9. **Stale-check warning and the status bar.** §6's row for stale-check panics ("`sources … failed`") predates the §5 ruling (commit 983cf0f1: warning Diagnostic, fail open). The warning lands in the diagnostics tile's config section, but `build_summary` counts only Error diagnostics [diagnostics.rs:593-603], so it does not reach the status bar. See Open questions.
10. **"Stopped threads" is a block at the top of the Sources section**, not a sixth diagnostics section. The stopped segment's click opens the tile on its default section, Sources [geode-diagnostics tile.rs:127]; a separate section would need a second step to see why the bar went red.
11. **Query-pool delivery boundary location.** The `DataEvent` is built in the service's `ResultSink` closure [service.rs:713-765], not in the pool. The build moves into `contained_result_event` (its own boundary); the pool's call of the sink is the channel send, outside the worker's boundary, and the worker is supervised. The pool's `ResultSink` API is unchanged.
12. **Arms with no production panic route** use the injected probe: Query, Document, Distinct, Series, Price, Upload (after Task 3 moves `kind.write`), Publish, Forget, Identities, Cancel, the view-replacement step, and loop death. Catalog and Fetch have a production route: a coverage row whose timestamp is past chrono's range panics `store::series::from_micros` (`.expect("a stored timestamp is in range")`) inside both `series_catalog` and `coverage` [series.rs:36-38, 343-344, 376-379]; the spec's "catalog over a malformed row" is this coverage row.
13. **Pricer store trait.** `SheetStore::save`/`forget` return `Result<(), Refusal>` and `Loaded::Refused` carries the `Refusal`, so the pricer tile can word and gate by kind [pricer store.rs:28-49].
14. **Status placement.** "Lead the left side" is read literally: the stopped segment is the first left child, before the count prefix.

## Owner rulings (Matthew, 2026-09-27) — these override the task text below

- **The stale-check report is an Error `Diagnostic`, not a Warning.** A catalog row the lookup cannot read is corruption even though the load proceeds (fail-open stays). As an Error it reaches the status bar's existing `data N errors` segment. Wherever Task 5 and Task 8 say "warning" for the stale check, read "error":
  - the diagnostic's `severity: Severity::Error`;
  - its test is named `a_failed_stale_check_is_an_error_diagnostic_through_the_service` and finds `Severity::Error`;
  - its mutation entry's replacement turns `Error` into `Warning`, so the named test fails;
  - the data-path doc says "error diagnostic naming the file; it counts in `data N errors`".

  Deviation 9 is resolved by this. The summary still counts only errors.
- **`cancel` keeps `-> bool`** (deviation 7 accepted).

---

## File Structure

| File | Responsibility | Tasks |
|---|---|---|
| `crates/geode-data/src/supervise.rs` (new) | `spawn_supervised`, `REQUEST_LOOP`, `unwatched`, test sink helpers | 1, 2 |
| `crates/geode-data/src/handle.rs` | `Refusal`, `stopped` flag, `send`, `serve` containment, probe, `PanicAnswer`, `StoppedOnUnwind` | 1, 4 |
| `crates/geode-data/src/service.rs` | `DataEvent::ThreadStopped`, worker stop sinks, `fail_fetch`, `health` field, result-event containment, identities/stale-check mapping, `replace_views` order | 1, 2, 4, 5 |
| `crates/geode-data/src/lib.rs` | module + `Refusal` export | 1 |
| `crates/geode-data/src/query/pool.rs`, `pricing/worker.rs`, `ingest/runner.rs`, `ingest/scheduler.rs`, `ingest/fetch.rs`, `ingest/subscribe.rs`, `egress.rs` | supervised spawns; runner/scheduler/fetch no-data sites; egress encoding on the worker | 2, 3, 5 |
| `crates/geode-app/src/events.rs` | `Key::Stopped` | 1 |
| `crates/geode-app/src/bridge.rs` | `ThreadStopped` fold, refused read, distinct/catalog/replace_views refusals | 1, 6, 7 |
| `crates/geode-shell/src/diagnostics.rs` | `StoppedThread`, `StoppedSegment`, `note_thread_stopped`, `note_refused`, summary | 6 |
| `crates/geode-shell/src/shell/status.rs`, `shell/render.rs` | the stopped segment | 6 |
| `crates/geode-diagnostics/src/sections.rs` | stopped-threads block | 6 |
| `crates/geode-blotter/src/tile.rs`, `geode-marketdata/src/tile.rs`, `geode-timeseries/src/tile/data.rs` (+ `tile/tests.rs`), `geode-pricer/src/tile.rs`, `geode-pricer/src/store.rs` | refusal wording and retry by kind | 1 (mechanical), 7 |
| `docs/current/data-path.md`, `request-delivery.md`, `shell.md`, `crates/geode-data/README.md`, `crates/geode-shell/README.md`, `crates/geode-pricer/README.md` | docs | 8 |
| `scripts/mutation-check.sh` | entries | every task |

---

### Task 1: Refusal, the stopped flag, `spawn_supervised`, and `ThreadStopped`

**Why one task:** the `bool` → `Result<(), Refusal>` change must land with every caller or the workspace does not compile. Callers are adapted mechanically here (`.is_ok()` / `.is_err()`) so behaviour outside `geode-data` is unchanged; Task 7 gives them per-kind behaviour. The pricer's `SheetStore` trait is changed properly here because its anchored bodies (`self.data.publish(..)`, `self.data.forget(..)`) are then untouched.

**Files:**
- Create: `crates/geode-data/src/supervise.rs`
- Modify: `crates/geode-data/src/lib.rs`, `crates/geode-data/src/handle.rs`, `crates/geode-data/src/service.rs` (enum + one test line), `crates/geode-app/src/events.rs`, `crates/geode-app/src/bridge.rs`, `crates/geode-blotter/src/tile.rs`, `crates/geode-marketdata/src/tile.rs`, `crates/geode-timeseries/src/tile/data.rs`, `crates/geode-pricer/src/tile.rs`, `crates/geode-pricer/src/store.rs`, `scripts/mutation-check.sh`

**Interfaces:**
- Produces:
  - `pub enum geode_data::Refusal { Busy, Stopped }` — `Debug, Clone, Copy, PartialEq, Eq, Display`.
  - `DataHandle::{query, distinct, document, upload, series, catalog, price, publish, forget, fetch, identities}(&self, ..) -> Result<(), Refusal>`; `replace_views(&self, Vec<ViewSpec>, DerivedDimensions) -> Result<(), Refusal>`; `cancel(&self, QueryKey) -> bool` (unchanged).
  - `#[cfg(any(test, feature = "test-support"))] DataHandle::fill_for_tests(&self)` — fills the queue so the next submission is `Err(Busy)`.
  - `DataEvent::ThreadStopped { thread: String, reason: String }`.
  - `geode_data::supervise::REQUEST_LOOP: &str = "geode-data"`; `pub(crate) fn spawn_supervised(name: String, sink: EventSink, body: impl FnOnce() + Send + 'static) -> std::io::Result<JoinHandle<()>>`.
  - `#[cfg(test)] pub(crate) mod supervise::tests_support { pub(crate) fn recording() -> (EventSink, Receiver<DataEvent>); pub(crate) fn next_stop(rx: &Receiver<DataEvent>) -> (String, String) }`.
  - `serve(config, sink, rx, pending_views, stopped: Arc<AtomicBool>)` (Task 4 adds a probe).
  - `pricer::store::SheetStore::{save, forget} -> Result<(), Refusal>`; `Loaded::Refused(Refusal)`.

- [ ] **Step 1: Write the supervision module with its failing tests**

Create `crates/geode-data/src/supervise.rs`:

```rust
//! One door for every long-lived data thread. A body that unwinds past
//! every containment boundary is declared once, with its payload, as
//! `DataEvent::ThreadStopped`, and nothing restarts it: a panic that repeats
//! on every request would otherwise crash-loop. The body runs without the
//! `contained` marker, so the app's panic hook still writes a crash file; an
//! uncontained thread death is a bug and the file is its report. A body that
//! returns is a deliberate stop and declares nothing.
//!
//! Every long-lived thread this crate spawns goes through
//! [`spawn_supervised`]; a new one must too, or its death is silent.

use crate::service::{DataEvent, EventSink};
use std::thread::JoinHandle;

/// The request loop's thread name. The shell repeats it (it cannot depend on
/// this crate) to label the loop as the data service.
pub const REQUEST_LOOP: &str = "geode-data";

/// Spawn `body` on a thread called `name`. If `body` unwinds, emit exactly one
/// `ThreadStopped` naming the thread and the payload, then end the thread
/// normally so a later join does not propagate the panic.
pub(crate) fn spawn_supervised(
    name: String,
    sink: EventSink,
    body: impl FnOnce() + Send + 'static,
) -> std::io::Result<JoinHandle<()>> {
    let thread = name.clone();
    std::thread::Builder::new().name(name).spawn(move || {
        // Not `contained`: the panic hook must treat this unwind as a crash.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));
        if let Err(payload) = outcome {
            let reason = crate::ingest::runner::panic_payload_message(payload.as_ref());
            tracing::error!(target: "geode::ingest", "data thread {thread} stopped: {reason}");
            let _ = sink(DataEvent::ThreadStopped { thread, reason });
        }
    })
}

#[cfg(test)]
pub(crate) mod tests_support {
    use crate::service::{DataEvent, EventSink};
    use std::sync::Arc;
    use std::sync::mpsc::{Receiver, channel};
    use std::time::Duration;

    /// A sink that keeps every event for the test to read.
    pub(crate) fn recording() -> (EventSink, Receiver<DataEvent>) {
        let (tx, rx) = channel();
        let tx = std::sync::Mutex::new(tx);
        (Arc::new(move |e| tx.lock().unwrap().send(e).is_ok()), rx)
    }

    /// The next `ThreadStopped`, skipping everything else.
    pub(crate) fn next_stop(rx: &Receiver<DataEvent>) -> (String, String) {
        loop {
            match rx.recv_timeout(Duration::from_secs(30)).expect("a ThreadStopped") {
                DataEvent::ThreadStopped { thread, reason } => return (thread, reason),
                _ => continue,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::recording;
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn a_panicking_supervised_body_emits_one_thread_stopped() {
        let (sink, rx) = recording();
        spawn_supervised("geode-test".into(), sink, || panic!("supervised boom"))
            .unwrap()
            .join()
            .expect("the helper catches the unwind");
        let events: Vec<DataEvent> = rx.try_iter().collect();
        assert_eq!(events.len(), 1, "{events:?}");
        match &events[0] {
            DataEvent::ThreadStopped { thread, reason } => {
                assert_eq!(thread, "geode-test");
                assert!(reason.contains("supervised boom"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_returning_supervised_body_declares_nothing() {
        let (sink, rx) = recording();
        spawn_supervised("geode-test".into(), sink, || {})
            .unwrap()
            .join()
            .unwrap();
        assert!(rx.try_iter().next().is_none());
    }

    #[test]
    fn the_supervised_body_is_not_marked_contained() {
        let (sink, _rx) = recording();
        let marked = Arc::new(AtomicBool::new(true));
        let seen = Arc::clone(&marked);
        spawn_supervised("geode-test".into(), sink, move || {
            seen.store(geode_core::panic::is_contained(), Ordering::SeqCst)
        })
        .unwrap()
        .join()
        .unwrap();
        assert!(
            !marked.load(Ordering::SeqCst),
            "the crash hook must see an uncontained thread death as a crash"
        );
    }
}
```

In `crates/geode-data/src/lib.rs` add `pub mod supervise;` after `pub mod store;`, and change the handle export to `pub use handle::{DataHandle, REQUEST_BOUND, Refusal, Request};`.

In `crates/geode-data/src/service.rs`, add the variant at the end of `pub enum DataEvent` (after `ForgetFailed { .. }`):

```rust
    /// A data thread unwound past every containment boundary and has ended.
    /// Emitted once per thread; nothing restarts it. `thread` is the spawn
    /// name (`geode-data`, `geode-ingest`, `geode-query-2`, ...), `reason` the
    /// panic payload, or the open error for a request loop that never started.
    ThreadStopped { thread: String, reason: String },
```

- [ ] **Step 2: Run the supervision tests**

Run: `cargo test -p geode-data --lib supervise`
Expected: PASS (3 tests). The helper and its tests land together; the failing half of this task's cycle is Step 4. `geode-app` does not compile until Step 6 (the new `DataEvent` variant); that is expected here.

- [ ] **Step 3: Write the failing handle tests**

In `crates/geode-data/src/handle.rs` tests module, replace `a_full_channel_refuses_and_counts_rather_than_blocking` and `a_gone_service_thread_refuses_every_request` with:

```rust
    #[test]
    fn a_full_channel_refuses_and_counts_rather_than_blocking() {
        // Fill the bounded channel without draining it to verify refusal and its
        // counter without relying on service-thread timing.
        let (h, _rx) = DataHandle::for_tests();
        let mut accepted = 0;
        for i in 0..(REQUEST_BOUND as u64 + 5) {
            if h.query(params(i, "tree")).is_ok() {
                accepted += 1;
            }
        }
        assert_eq!(accepted, REQUEST_BOUND);
        assert_eq!(h.dropped_requests(), 5);
        assert_eq!(
            h.query(params(99, "tree")),
            Err(Refusal::Busy),
            "a full queue passes: a later submission can succeed"
        );
    }

    #[test]
    fn a_gone_service_thread_refuses_every_request() {
        let (h, rx) = DataHandle::for_tests();
        drop(rx);
        assert_eq!(h.query(params(1, "tree")), Err(Refusal::Stopped));
        assert!(!h.cancel(QueryKey(1)));
        assert_eq!(
            h.dropped_requests(),
            0,
            "a stopped service is not a busy one; only Busy counts"
        );
    }

    #[test]
    fn fill_for_tests_makes_the_next_submission_busy() {
        let (h, _rx) = DataHandle::for_tests();
        h.fill_for_tests();
        assert_eq!(h.series(series_params(3)), Err(Refusal::Busy));
    }
```

and add, next to `a_series_request_is_queued_as_a_request`, a helper built from that test's literal:

```rust
    fn series_params(key: u64) -> SeriesParams {
        use geode_core::series::{BucketRule, Frequency, SeriesSpec, SlotKind};
        SeriesParams {
            key: QueryKey(key),
            tag: 5,
            submitted: Instant::now(),
            dataset: "series".into(),
            range: (ts("2026-01-05T00:00:00Z"), ts("2026-01-06T00:00:00Z")),
            window: (ts("2026-01-05T00:00:00Z"), ts("2026-01-06T00:00:00Z")),
            as_of: AsOf::Live,
            frequency: Frequency::D1,
            series: vec![SeriesSpec {
                slot: 1,
                kind: SlotKind::Source {
                    source: "k".into(),
                    identity: "SPX".into(),
                    rule: BucketRule::Last,
                },
            }],
            percentiles: Vec::new(),
            bins: None,
        }
    }
```

(and make `a_series_request_is_queued_as_a_request` call `handle.series(series_params(3))`).

In `the_real_service_answers_through_the_sink_and_reports_open_failures`, replace the post-shutdown block with:

```rust
        h.shutdown();
        assert_eq!(
            h.query(params(11, "tree")),
            Err(Refusal::Stopped),
            "after shutdown nothing is accepted, and nothing will be"
        );
        assert_eq!(
            h.dropped_requests(),
            0,
            "a refusal after shutdown is not a busy one"
        );
```

Replace `an_unopenable_database_is_a_diagnostic_not_a_panic` with:

```rust
    #[test]
    fn an_unopenable_database_is_a_diagnostic_and_a_stopped_request_loop() {
        let (tx, rx) = channel();
        let sink: EventSink = Arc::new(move |e| tx.send(e).is_ok());
        let h = DataService::spawn(
            DataServiceConfig {
                // A directory, not a file: DuckDB cannot open it.
                db_path: std::env::temp_dir(),
                schema: geode_core::schema::SchemaSpec::default(),
                views: Vec::new(),
                dimensions: geode_core::dimensions::DerivedDimensions::default(),
                query_workers: 1,
                sources: Vec::new(),
                adapters: crate::adapter::AdapterRegistry::default(),
                documents: crate::documents::DocumentRegistry::default(),
                egress: Vec::new(),
                pricer: crate::pricing::PricerConfig::default(),
            },
            sink,
        );
        match rx.recv_timeout(Duration::from_secs(30)).unwrap() {
            DataEvent::Diagnostics(d) => {
                assert!(d.iter().any(|d| d.message.contains("open")), "{d:?}")
            }
            other => panic!("{other:?}"),
        }
        // Set before the diagnostic was sent: the next submission already
        // says the service is gone, with no retry worth making.
        assert_eq!(h.query(params(1, "tree")), Err(Refusal::Stopped));
        match rx.recv_timeout(Duration::from_secs(30)).unwrap() {
            DataEvent::ThreadStopped { thread, reason } => {
                assert_eq!(thread, crate::supervise::REQUEST_LOOP);
                assert!(reason.contains("failed to open"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
    }
```

In every other `handle.rs` test, adapt the `bool` assertions: `assert!(x.method(..))` becomes `assert!(x.method(..).is_ok())`; `assert!(!x.method(..))` becomes an `assert_eq!(x.method(..), Err(Refusal::…))` with the kind the setup produces (a full queue → `Busy`; a shut-down or dropped receiver → `Stopped`), e.g. in `upload_is_refused_when_the_request_channel_is_full`: `assert_eq!(handle.upload(upload_params(1)), Err(Refusal::Busy));`; in `view_reload_survives_a_full_request_queue_and_keeps_the_latest`: `assert!(handle.replace_views(..).is_ok())` twice and, at the end, `assert_eq!(handle.replace_views(Vec::new(), DerivedDimensions::default()), Err(Refusal::Stopped));`. `cancel` stays `bool`. In `shutdown_completes_even_when_the_request_queue_is_full` leave `h.cancel(QueryKey(1));` as is. In `crates/geode-data/src/service.rs` tests change `assert!(handle.series(p));` to `assert!(handle.series(p).is_ok());`.

Add the mailbox test to `crates/geode-app/src/events.rs` tests:

```rust
    /// Each thread stops once, and two different ones stopping between two
    /// drains must both reach the status bar.
    #[gpui::test]
    async fn two_threads_stopping_between_drains_are_both_delivered() {
        let (tx, rx) = channel();
        for thread in ["geode-ingest", "geode-discovery"] {
            tx.try_send(DataEvent::ThreadStopped {
                thread: thread.into(),
                reason: "boom".into(),
            })
            .unwrap();
        }
        for expected in ["geode-ingest", "geode-discovery"] {
            assert!(matches!(
                rx.recv().await.unwrap(),
                DataEvent::ThreadStopped { thread, .. } if thread == expected
            ));
        }
    }
```

- [ ] **Step 4: Run to verify they fail**

Run: `cargo test -p geode-data --lib handle`
Expected: FAIL to compile (`Refusal` not found, `fill_for_tests` not found, `serve` signature).

- [ ] **Step 5: Implement the handle changes**

In `crates/geode-data/src/handle.rs`:

Imports: `use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};` and `use crate::supervise::REQUEST_LOOP;`.

Add after `REQUEST_BOUND`:

```rust
/// Why a submission was not admitted. `Busy` passes: the request queue was
/// full and a later submission can succeed. `Stopped` does not: the request
/// loop has ended (a panic, a failed open, or shutdown) and nothing will serve
/// a later one, so retrying only repeats the refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    Busy,
    Stopped,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Refusal::Busy => "the data service is busy",
            Refusal::Stopped => "the data service has stopped",
        })
    }
}
```

`Inner` gains a field after `dropped`:

```rust
    /// Set when the request loop can no longer serve: it failed to open, or
    /// it is unwinding. Read before the channel so a submission racing a
    /// dying loop is refused `Stopped` rather than admitted to a queue that
    /// nothing will read. A deliberate shutdown does not set it.
    stopped: Arc<AtomicBool>,
```

Replace `Inner::send`:

```rust
    fn send(&self, req: Request) -> Result<(), Refusal> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(Refusal::Stopped);
        }
        let guard = self.tx.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_ref() {
            Some(tx) => match tx.try_send(req) {
                Ok(()) => Ok(()),
                Err(TrySendError::Full(_)) => {
                    self.dropped.fetch_add(1, Ordering::Relaxed);
                    Err(Refusal::Busy)
                }
                Err(TrySendError::Disconnected(_)) => Err(Refusal::Stopped),
            },
            None => Err(Refusal::Stopped),
        }
    }
```

`DataHandle::send` returns `Result<(), Refusal>`. Every submit method returns `Result<(), Refusal>` with its doc's "`false` means …" rewritten to "`Err(Busy)` means the queue was full and a later submission can succeed; `Err(Stopped)` means the service can no longer serve and no outcome is owed". `cancel` stays:

```rust
    /// Queue cancellation for query-pool and pricing work under this key.
    /// `false` means it was not queued; a stale answer that still arrives is
    /// dropped by its receiver's tag check. There is no acknowledgement; this
    /// does not cancel fetches or ingest jobs, or retract emitted results.
    pub fn cancel(&self, key: QueryKey) -> bool {
        self.send(Request::Cancel { key }).is_ok()
    }
```

`replace_views`:

```rust
    /// Store the latest views and dimensions, replacing any pending replacement.
    /// A full wakeup queue still answers `Ok`: the service checks the mailbox
    /// before every dequeued request, so the only refusal is `Stopped`. `Ok`
    /// acknowledges retained state, not validation or application; validation
    /// diagnostics return through the event sink.
    pub fn replace_views(
        &self,
        views: Vec<ViewSpec>,
        dimensions: DerivedDimensions,
    ) -> Result<(), Refusal> {
        if self.inner.stopped.load(Ordering::Acquire) {
            return Err(Refusal::Stopped);
        }
        let guard = self.inner.tx.lock().unwrap_or_else(|e| e.into_inner());
        let Some(tx) = guard.as_ref() else {
            return Err(Refusal::Stopped);
        };
        let mut pending = self
            .inner
            .pending_views
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *pending = Some(ViewReplacement { views, dimensions });
        match tx.try_send(Request::ReplaceViews) {
            Ok(()) | Err(TrySendError::Full(_)) => Ok(()),
            Err(TrySendError::Disconnected(_)) => {
                pending.take();
                Err(Refusal::Stopped)
            }
        }
    }
```

`dropped_requests` doc: "Submissions refused `Busy` so far (a full request queue). `Stopped` refusals are not counted: they describe a service that is gone, not one that is behind."

Add the test helper beside `for_tests` (and give `for_tests`'s `Inner` `stopped: Arc::default()`):

```rust
    /// Fill the request queue so the next submission is refused `Busy`, the
    /// refusal a burst produces. The test holding the paired receiver drains
    /// it to admit again.
    #[cfg(any(test, feature = "test-support"))]
    pub fn fill_for_tests(&self) {
        while self.cancel(QueryKey(u64::MAX)) {}
    }
```

`DataService::spawn`:

```rust
    pub fn spawn(config: DataServiceConfig, sink: EventSink) -> DataHandle {
        let (tx, rx) = sync_channel(REQUEST_BOUND);
        let pending_views = PendingViews::default();
        let service_views = Arc::clone(&pending_views);
        let stopped = Arc::new(AtomicBool::new(false));
        let loop_stopped = Arc::clone(&stopped);
        let loop_sink = Arc::clone(&sink);
        let thread = crate::supervise::spawn_supervised(REQUEST_LOOP.to_string(), sink, move || {
            serve(config, loop_sink, rx, service_views, loop_stopped)
        })
        .expect("spawning the data service thread");
        DataHandle {
            inner: Arc::new(Inner {
                pending_views,
                tx: Mutex::new(Some(tx)),
                thread: Mutex::new(Some(thread)),
                dropped: AtomicU64::new(0),
                stopped,
            }),
        }
    }
```

`serve` gains `stopped: Arc<AtomicBool>` as its last parameter; its open-failure arm becomes:

```rust
        Err(e) => {
            // The service never became available: refuse later submissions
            // `Stopped` from now, and declare the loop gone although nothing
            // unwound, so the status bar says so.
            stopped.store(true, Ordering::Release);
            let reason = format!("data service failed to open: {e}");
            let _ = sink(DataEvent::Diagnostics(vec![Diagnostic {
                severity: Severity::Error,
                layer: None,
                file: None,
                message: reason.clone(),
                path: None,
            }]));
            let _ = sink(DataEvent::ThreadStopped { thread: REQUEST_LOOP.to_string(), reason });
            return;
        }
```

Update the direct call in `view_reload_survives_a_full_request_queue_and_keeps_the_latest` to `serve(config, sink, requests, pending, Arc::default())`.

- [ ] **Step 6: Adapt the other crates mechanically**

`crates/geode-app/src/events.rs`: add `Stopped(String),` to `enum Key` with the doc "`ThreadStopped`, keyed by thread: each thread stops once, and two stopping before a drain must both be delivered." and the `key` arm `DataEvent::ThreadStopped { thread, .. } => Key::Stopped(thread.clone()),`.

`crates/geode-app/src/bridge.rs`:
- `if handle.catalog(CatalogParams { .. }) {` → `if handle.catalog(CatalogParams { .. }).is_ok() {`
- `handle.replace_views(views, dims);` → `let _ = handle.replace_views(views, dims);`
- `let queued = handle.distinct(params.clone());` → `let queued = handle.distinct(params.clone()).is_ok();` (the anchored `if !queued {` line stays)
- in the drain `match event`, a temporary arm (Task 6 replaces it):
  ```rust
                    DataEvent::ThreadStopped { thread, reason } => {
                        tracing::error!(target: "geode::shell", "data thread {thread} stopped: {reason}");
                    }
  ```
- tests: `assert!(handle.document(..))` → `assert!(handle.document(..).is_ok())`.

`crates/geode-blotter/src/tile.rs`: `if !queued {` (after `let queued = self.data.query(..)`) → `if queued.is_err() {`.

`crates/geode-marketdata/src/tile.rs`: document site `self.query_in_flight = queued;` → `self.query_in_flight = queued.is_ok();` and its `if !queued {` → `if queued.is_err() {`; upload site `if !queued {` → `if queued.is_err() {`.

`crates/geode-timeseries/src/tile/data.rs`: fetch site `if queued {` → `if queued.is_ok() {`; series site `let queued = self.data.series(params);` → `let queued = self.data.series(params).is_ok();`.

`crates/geode-pricer/src/store.rs`:
- `use geode_data::{DataHandle, LocalForget, Refusal};`
- `Loaded::Refused` → `Refused(Refusal)`, doc: "The load was never submitted, and why: `Busy` (a full request channel) or `Stopped` (the service is gone). No answer will arrive. The caller must report a failed load and block saves so an empty fallback cannot overwrite the stored document."
- trait: `fn save(&self, name: &str, rows: DocumentRows) -> Result<(), Refusal>;` (doc: "`Err`: refused, nothing written, and why.") and `fn forget(&self, name: &str) -> Result<(), Refusal>;`.
- `MemorySheetStore::load`: `return Loaded::Refused(Refusal::Busy);`; `save`: `if self.refusing.get() { return Err(Refusal::Busy); }` … `Ok(())`; `forget`: `Ok(())`.
- `DuckSheetStore::load` tail becomes:
  ```rust
        match queued {
            Ok(()) => Loaded::Pending,
            Err(refusal) => Loaded::Refused(refusal),
        }
  ```
  `save` and `forget` bodies are unchanged (they now return the handle's `Result`).
- tests: `a_closed_channel_answers_refused_not_pending` asserts `Loaded::Refused(Refusal::Stopped)`; `assert!(store.save(..))`/`assert!(store.forget(..))` gain `.is_ok()`; statement calls `a.save("book", rows());` become `a.save("book", rows()).unwrap();`.

`crates/geode-pricer/src/tile.rs`:
- `if self.shared.store.save(&self.sheet.name, rows) {` → `if self.shared.store.save(&self.sheet.name, rows).is_ok() {`
- `} else if self.shared.store.forget(&old) {` → `} else if self.shared.store.forget(&old).is_ok() {`
- `} else if self.shared.store.forget(&pending.sheet) {` → `} else if self.shared.store.forget(&pending.sheet).is_ok() {`
- both `Loaded::Refused =>` arms → `Loaded::Refused(_) =>`
- `if queued {` after `let queued = self.data.price(..)` → `if queued.is_ok() {`
- tests: every `assert!(store.save(..))` / `assert!(h.store.save(..))` gains `.is_ok()`; a statement `h.store.save(..)` becomes `.unwrap()`.

Then run `cargo check --workspace --all-targets` and fix each remaining `E0308`/`unused_must_use` the same way (the compiler lists them; there are no other production callers).

- [ ] **Step 7: Run tests to verify they pass**

Run: `cargo test -p geode-data --lib handle` then `cargo test -p geode-data --lib supervise` then `cargo test -p geode-app --lib events`
Expected: PASS.

- [ ] **Step 8: Gate and commit**

Run: `cargo test -p geode-data && cargo test -p geode-app && cargo test -p geode-pricer && cargo test -p geode-blotter && cargo test -p geode-marketdata && cargo test -p geode-timeseries`, then `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check`, `zsh scripts/mutation-check.sh --anchors-only`.

```bash
git add -A crates/geode-data crates/geode-app crates/geode-blotter crates/geode-marketdata crates/geode-timeseries crates/geode-pricer
git commit -m "feat(data): Refusal, supervised request loop and ThreadStopped

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

- [ ] **Step 9: Mutation entries**

Existing entries on changed lines: `handle: a refused request is counted` (anchor `                    self.dropped.fetch_add(1, Ordering::Relaxed);` still matches once, in `send`'s `Full` arm — confirm with `--anchors-only`); `pricer store: a refused submission answers Refused, not Pending` — re-aim:

```
run_mutation "pricer store: a refused submission answers Refused, not Pending" \
  crates/geode-pricer/src/store.rs \
  '        match queued {
            Ok(()) => Loaded::Pending,
            Err(refusal) => Loaded::Refused(refusal),
        }' \
  '        let _ = queued;
        Loaded::Pending' \
  geode-pricer a_closed_channel_answers_refused_not_pending
```

New entries (containment section):

```
# A body that unwinds must be declared: otherwise a dead data thread is
# invisible and every tile waits on it forever.
run_mutation "supervise: an unwinding body is not declared" \
  crates/geode-data/src/supervise.rs \
  '            let _ = sink(DataEvent::ThreadStopped { thread, reason });' \
  '            let _ = (&sink, thread, reason);' \
  geode-data a_panicking_supervised_body_emits_one_thread_stopped

# Marked contained, the crash hook would log the death and write no crash
# file: the one bug report an uncontained death leaves.
run_mutation "supervise: the body runs contained" \
  crates/geode-data/src/supervise.rs \
  '        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));' \
  '        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| geode_core::panic::contained(body)));' \
  geode-data the_supervised_body_is_not_marked_contained

run_mutation "handle: a full queue is refused Stopped" \
  crates/geode-data/src/handle.rs \
  '                Err(TrySendError::Full(_)) => {
                    self.dropped.fetch_add(1, Ordering::Relaxed);
                    Err(Refusal::Busy)' \
  '                Err(TrySendError::Full(_)) => {
                    self.dropped.fetch_add(1, Ordering::Relaxed);
                    Err(Refusal::Stopped)' \
  geode-data a_full_channel_refuses_and_counts_rather_than_blocking

run_mutation "handle: a disconnected queue is refused Busy" \
  crates/geode-data/src/handle.rs \
  '                Err(TrySendError::Disconnected(_)) => Err(Refusal::Stopped),' \
  '                Err(TrySendError::Disconnected(_)) => Err(Refusal::Busy),' \
  geode-data a_gone_service_thread_refuses_every_request

run_mutation "handle: a shut-down handle is refused Busy" \
  crates/geode-data/src/handle.rs \
  '            None => Err(Refusal::Stopped),' \
  '            None => Err(Refusal::Busy),' \
  geode-data the_real_service_answers_through_the_sink_and_reports_open_failures

run_mutation "serve: an open failure is not declared" \
  crates/geode-data/src/handle.rs \
  '            let _ = sink(DataEvent::ThreadStopped { thread: REQUEST_LOOP.to_string(), reason });' \
  '            let _ = reason;' \
  geode-data an_unopenable_database_is_a_diagnostic_and_a_stopped_request_loop

run_mutation "events: two threads stopping coalesce into one" \
  crates/geode-app/src/events.rs \
  '        DataEvent::ThreadStopped { thread, .. } => Key::Stopped(thread.clone()),' \
  '        DataEvent::ThreadStopped { .. } => Key::Stopped(String::new()),' \
  geode-app two_threads_stopping_between_drains_are_both_delivered
```

The open-failure `stopped.store(..)` line gets no entry: the receiver drops on the same return, so a mutation removing it is only observable through a race.

Verify each per Global Constraints, then:

```bash
git add scripts/mutation-check.sh
git commit -m "test(mutation): supervision and refusal entries

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: Supervise every other long-lived data thread

**Files:**
- Modify: `crates/geode-data/src/supervise.rs`, `query/pool.rs`, `pricing/worker.rs`, `ingest/runner.rs`, `ingest/scheduler.rs`, `ingest/fetch.rs`, `ingest/subscribe.rs`, `egress.rs`, `service.rs`, `consistency_tests.rs`, `scripts/mutation-check.sh`
- Test: in each of those modules.

**Interfaces:**
- Consumes: `spawn_supervised`, `tests_support::{recording, next_stop}` (Task 1).
- Produces:
  - `pub(crate) fn supervise::unwatched() -> EventSink`.
  - `QueryPool::spawn_with_sink(store, workers, sink: ResultSink, stop: EventSink)`; private `spawn_with_run(store, workers, sink, stop, run)`.
  - `PricingWorker::spawn(config, sink: PriceSink, stop: EventSink)`.
  - `IngestRunner::spawn(store, schema, sink: IngestSink, stop: EventSink)`; private `spawn_with(store, schema, sink, stop, load, publish)`.
  - `Scheduler::spawn(sources, conn, ingest, sink: SchedulerSink, stop: EventSink)`.
  - `FetchWorker::spawn(source, fetch, sink: FetchOutcomeSink, stop: EventSink)`.
  - `SubscriptionWorker::spawn(spec, dataset, kind, subscription, ingest, report_load, on_connection, stop: EventSink)`.
  - Thread names unchanged: `geode-query-{i}`, `geode-pricing`, `geode-ingest`, `geode-discovery`, `geode-fetch-{source}`, `geode-subscribe-{source}`, `geode-egress-{target}`.

Every wiring test kills its worker through a sink call that sits outside the worker's own boundaries — the one way these threads can still die — and waits for `ThreadStopped` naming the thread.

- [ ] **Step 1: Write the failing wiring tests**

`crates/geode-data/src/query/pool.rs` tests:

```rust
    #[test]
    fn a_query_worker_that_dies_is_declared_by_its_thread_name() {
        let (_d, store) = fixture(10);
        let (stop, stops) = crate::supervise::tests_support::recording();
        let sink: ResultSink = Arc::new(|_| panic!("the result sink fell over"));
        let pool = QueryPool::spawn_with_sink(&store, 1, sink, stop).unwrap();
        pool.submit(request(1, "v1", "select sum(v) as v from t"));
        let (thread, reason) = crate::supervise::tests_support::next_stop(&stops);
        assert_eq!(thread, "geode-query-0");
        assert!(reason.contains("the result sink fell over"), "{reason}");
        pool.shutdown();
    }
```

`crates/geode-data/src/pricing/worker.rs` tests:

```rust
    #[test]
    fn a_pricing_worker_that_dies_is_declared() {
        let (stop, stops) = crate::supervise::tests_support::recording();
        let sink: PriceSink = Arc::new(|_| panic!("the price sink fell over"));
        let w = PricingWorker::spawn(PricerConfig::missing("vendor"), sink, stop);
        assert!(w.request(params(1, 1, &["SPX"])));
        let (thread, reason) = crate::supervise::tests_support::next_stop(&stops);
        assert_eq!(thread, "geode-pricing");
        assert!(reason.contains("the price sink fell over"), "{reason}");
        w.shutdown();
    }
```

`crates/geode-data/src/ingest/runner.rs` tests (the runner announces `PlanComplete` at start, outside every boundary):

```rust
    #[test]
    fn an_ingest_runner_that_dies_is_declared() {
        let (_dir, store) = document_store();
        let (stop, stops) = crate::supervise::tests_support::recording();
        let sink: IngestSink = Arc::new(|_| panic!("the ingest sink fell over"));
        let handle = IngestRunner::spawn(store, schema_of(cvi_dataset()), sink, stop);
        let (thread, reason) = crate::supervise::tests_support::next_stop(&stops);
        assert_eq!(thread, "geode-ingest");
        assert!(reason.contains("the ingest sink fell over"), "{reason}");
        handle.shutdown();
    }
```

`crates/geode-data/src/ingest/scheduler.rs` tests (a panicking sink inside the poll is caught; the failure arm's own sink call is not):

```rust
    #[test]
    fn a_discovery_thread_that_dies_is_declared() {
        let (_db, _dir, ingest, _ingest_rx, conn, spec, _ds) =
            harness(Duration::from_millis(50), Duration::ZERO);
        let (stop, stops) = crate::supervise::tests_support::recording();
        let sink: SchedulerSink = Arc::new(|_| panic!("the scheduler sink fell over"));
        let sched = Scheduler::spawn(vec![spec], conn, ingest, sink, stop);
        let (thread, reason) = crate::supervise::tests_support::next_stop(&stops);
        assert_eq!(thread, "geode-discovery");
        assert!(reason.contains("the scheduler sink fell over"), "{reason}");
        sched.shutdown();
    }
```

`crates/geode-data/src/ingest/fetch.rs` tests:

```rust
    #[test]
    fn a_fetch_worker_that_dies_is_declared() {
        let (stop, stops) = crate::supervise::tests_support::recording();
        let sink: FetchOutcomeSink = Arc::new(|_| panic!("the fetch sink fell over"));
        let mut w = FetchWorker::spawn(
            "demo_kdb",
            Box::new(FakeFetch {
                calls: Default::default(),
                n: 1,
                catalogue: None,
                fail_once: false,
            }),
            sink,
            stop,
        )
        .unwrap();
        assert!(w.request(FetchWork::Identities));
        let (thread, reason) = crate::supervise::tests_support::next_stop(&stops);
        assert_eq!(thread, "geode-fetch-demo_kdb");
        assert!(reason.contains("the fetch sink fell over"), "{reason}");
        w.shutdown();
    }
```

`crates/geode-data/src/ingest/subscribe.rs` tests (the parse panic is contained; reporting it goes through `report_load`, outside the boundary):

```rust
    #[test]
    fn a_receiver_that_dies_is_declared() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        let ds = cvi_dataset();
        store.apply_schema(&ds).unwrap();
        Catalog::new(store.writer()).ensure_tables().unwrap();
        let mut schema = SchemaSpec::default();
        schema.datasets.push(ds.clone());
        let (handle, _events) = IngestRunner::spawn_channel(store, schema);
        let (bus, feed) = ChannelAdapter::new("test_bus");
        let spec = SourceSpec {
            adapter: "test_bus".into(),
            document: Some("fake_cvi".into()),
            topics: vec!["cvi/>".into()],
            coalesce: Duration::ZERO,
            ..SourceSpec::directory("cvi", "cvi_params", Vec::new())
        };
        let (stop, stops) = crate::supervise::tests_support::recording();
        let mut worker = SubscriptionWorker::spawn(
            &spec,
            ds,
            Arc::new(PanickingKind::new()),
            bus.subscription().expect("the channel adapter subscribes"),
            Arc::new(handle),
            Arc::new(|_: &str, _: Health, _: String| panic!("the load report fell over")),
            Arc::new(|_: ConnectionState| {}),
            stop,
        )
        .expect("the bus is open");
        feed.publish("cvi/SPX.Z", b"anything".to_vec());
        let (thread, reason) = crate::supervise::tests_support::next_stop(&stops);
        assert_eq!(thread, "geode-subscribe-cvi");
        assert!(reason.contains("the load report fell over"), "{reason}");
        worker.shutdown();
    }
```

`crates/geode-data/src/egress.rs` tests (the answer is sent outside the transport boundary):

```rust
    #[test]
    fn an_egress_worker_that_dies_is_declared() {
        let (adapters, _adapter, _feed) = channel_registry();
        let (tx, rx) = channel();
        let tx = Mutex::new(tx);
        let sink: EventSink = Arc::new(move |e| match e {
            DataEvent::Upload(_) => panic!("the upload answer fell over"),
            other => tx.lock().unwrap().send(other).is_ok(),
        });
        let workers = EgressWorkers::spawn(&[dividend_spec()], &adapters, sink);
        workers.upload(params(1, TARGET, DIVIDEND, "XYZ"), &documents());
        let (thread, reason) = crate::supervise::tests_support::next_stop(&rx);
        assert_eq!(thread, "geode-egress-sophis");
        assert!(reason.contains("the upload answer fell over"), "{reason}");
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-data --lib dies_is_declared`
Expected: FAIL to compile (extra `stop` argument) — and, for egress (whose signature is unchanged), FAIL at runtime: `next_stop` times out with "a ThreadStopped".

- [ ] **Step 3: Implement**

`crates/geode-data/src/supervise.rs`, add:

```rust
/// The stop sink for constructors that deliver into a channel (tests,
/// benches): they have no event sink, so a death is still caught and logged
/// by [`spawn_supervised`] but announced to no one.
pub(crate) fn unwatched() -> EventSink {
    std::sync::Arc::new(|_| false)
}
```

`query/pool.rs`: `spawn_with_sink(store, workers, sink, stop: crate::service::EventSink)` forwards `stop` to `spawn_with_run(store, workers, sink, stop, run_one)`; `spawn` passes `crate::supervise::unwatched()`. Inside `spawn_with_run`:

```rust
            let spawned = store.reader().and_then(|conn| {
                let q = Arc::clone(&queue);
                let sink = Arc::clone(&sink);
                crate::supervise::spawn_supervised(
                    format!("geode-query-{i}"),
                    Arc::clone(&stop),
                    move || worker(conn, q, sink, run),
                )
                .map_err(|source| crate::store::StoreError::SpawnWorker { source })
            });
```

Test callers: `channel_pool_with` and the two `QueryPool::spawn_with_sink(&store, 1, sink)` calls gain `crate::supervise::unwatched()`.

`pricing/worker.rs`:

```rust
    pub fn spawn(config: PricerConfig, sink: PriceSink, stop: crate::service::EventSink) -> PricingWorker {
        let queue: Arc<(Mutex<Queue>, Condvar)> = Arc::default();
        let thread = {
            let queue = Arc::clone(&queue);
            crate::supervise::spawn_supervised(
                "geode-pricing".to_string(),
                stop,
                move || run(queue, config, sink),
            )
            .expect("spawn the pricing worker")
        };
        PricingWorker { queue, thread: Mutex::new(Some(thread)) }
    }
```

Its five test callers gain `crate::supervise::unwatched()`.

`ingest/runner.rs`: `spawn(store, schema, sink, stop)` → `spawn_with(store, schema, sink, stop, load_file, publish_document)`; `spawn_with` spawns:

```rust
        let thread = crate::supervise::spawn_supervised(
            "geode-ingest".to_string(),
            stop,
            move || run(store, schema, worker_queue, sink, load, publish),
        )
        .expect("spawning the ingest thread");
```

`spawn_channel` passes `crate::supervise::unwatched()`; the test calls `IngestRunner::spawn(store, schema_of(ds), sink)` and `IngestRunner::spawn_with(store, schema, sink, load, publish)` gain it too.

`ingest/scheduler.rs`: `spawn(sources, conn, ingest, sink, stop)`:

```rust
        let thread = crate::supervise::spawn_supervised(
            "geode-discovery".to_string(),
            stop,
            move || run(sources, conn, ingest, sink, worker_stop),
        )
        .expect("spawning the discovery thread");
```

Its nine test callers gain `crate::supervise::unwatched()`.

`ingest/fetch.rs`: `spawn(source, fetch, sink, stop)`:

```rust
        let name = format!("geode-fetch-{source}");
        let thread = crate::supervise::spawn_supervised(
            name.clone(),
            stop,
            move || run(fetch, rx, sink),
        )
        .map_err(|e| AdapterError {
            message: format!("spawning {name}: {e}"),
        })?;
```

Its three test callers and `consistency_tests.rs::fetch_panic_delivers_a_failure_outcome` gain `crate::supervise::unwatched()`.

`ingest/subscribe.rs`: add `#[allow(clippy::too_many_arguments)]` and the `stop: crate::service::EventSink` parameter; the spawn becomes:

```rust
        let thread = crate::supervise::spawn_supervised(
            format!("geode-subscribe-{}", spec.name),
            stop,
            move || receiving.run(rx, window),
        );
```

The `harness` test helper passes `crate::supervise::unwatched()`.

`egress.rs` (`EgressWorkers::spawn`):

```rust
                    let spawned = crate::supervise::spawn_supervised(
                        format!("geode-egress-{}", spec.name),
                        EventSink::clone(&sink),
                        move || work(name, egress, rx, worker_sink),
                    );
```

`service.rs` (`DataService::open`): pass `Arc::clone(&sink)` as the stop sink to `QueryPool::spawn_with_sink`, `PricingWorker::spawn`, `IngestRunner::spawn`, `FetchWorker::spawn`, `SubscriptionWorker::spawn`, `Scheduler::spawn`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p geode-data --lib dies_is_declared`
Expected: PASS (7 tests).

- [ ] **Step 5: Gate and commit**

Run: `cargo test -p geode-data`, `cargo clippy -p geode-data --all-targets -- -D warnings`, `cargo fmt --check`, `zsh scripts/mutation-check.sh --anchors-only`.

```bash
git add crates/geode-data
git commit -m "feat(data): every long-lived data thread is supervised

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

- [ ] **Step 6: Mutation entries**

No existing entry anchors a spawn line (checked: `grep -n "thread::Builder\|geode-query-\|geode-pricing\|geode-ingest\|geode-discovery\|geode-fetch\|geode-subscribe\|geode-egress" scripts/mutation-check.sh` finds none). Each new entry swaps the stop sink for `unwatched()`, so the worker is still caught but its death is announced to no one:

```
# A worker spawned without the service's sink dies unannounced.
run_mutation "supervise: a query worker's death is announced to no one" \
  crates/geode-data/src/query/pool.rs \
  '                    format!("geode-query-{i}"),
                    Arc::clone(&stop),' \
  '                    format!("geode-query-{i}"),
                    crate::supervise::unwatched(),' \
  geode-data a_query_worker_that_dies_is_declared_by_its_thread_name

run_mutation "supervise: the pricing worker's death is announced to no one" \
  crates/geode-data/src/pricing/worker.rs \
  '                "geode-pricing".to_string(),
                stop,' \
  '                "geode-pricing".to_string(),
                crate::supervise::unwatched(),' \
  geode-data a_pricing_worker_that_dies_is_declared

run_mutation "supervise: the ingest runner's death is announced to no one" \
  crates/geode-data/src/ingest/runner.rs \
  '            "geode-ingest".to_string(),
            stop,' \
  '            "geode-ingest".to_string(),
            crate::supervise::unwatched(),' \
  geode-data an_ingest_runner_that_dies_is_declared

run_mutation "supervise: discovery's death is announced to no one" \
  crates/geode-data/src/ingest/scheduler.rs \
  '            "geode-discovery".to_string(),
            stop,' \
  '            "geode-discovery".to_string(),
            crate::supervise::unwatched(),' \
  geode-data a_discovery_thread_that_dies_is_declared

run_mutation "supervise: a fetch worker's death is announced to no one" \
  crates/geode-data/src/ingest/fetch.rs \
  '            name.clone(),
            stop,' \
  '            name.clone(),
            crate::supervise::unwatched(),' \
  geode-data a_fetch_worker_that_dies_is_declared

run_mutation "supervise: a receiver's death is announced to no one" \
  crates/geode-data/src/ingest/subscribe.rs \
  '            format!("geode-subscribe-{}", spec.name),
            stop,' \
  '            format!("geode-subscribe-{}", spec.name),
            crate::supervise::unwatched(),' \
  geode-data a_receiver_that_dies_is_declared

run_mutation "supervise: an egress worker's death is announced to no one" \
  crates/geode-data/src/egress.rs \
  '                        format!("geode-egress-{}", spec.name),
                        EventSink::clone(&sink),' \
  '                        format!("geode-egress-{}", spec.name),
                        crate::supervise::unwatched(),' \
  geode-data an_egress_worker_that_dies_is_declared
```

Verify each; commit `test(mutation): worker supervision entries`.

---

### Task 3: `kind.write` runs on the egress worker

**Files:**
- Modify: `crates/geode-data/src/egress.rs`, `scripts/mutation-check.sh`
- Test: `crates/geode-data/src/egress.rs` tests

**Interfaces:**
- Consumes: `EgressWorkers::spawn` (supervised, Task 2).
- Produces: private `struct Job { key, tag, document, document_key, address, rows: DocumentRows, kind: Arc<dyn DocumentKind> }`; private `fn run_job(name: &str, egress: &mut dyn Egress, job: &Job) -> Result<(), String>`. `EgressWorkers::upload` signature unchanged. After this task no production input can panic the request loop's `Upload` arm (Task 4 probes it).

- [ ] **Step 1: Write the failing test**

Extend the test `KeyKind::write` so a key of `PANIC` panics:

```rust
        fn write(&self, rows: &DocumentRows) -> Result<Vec<u8>, WriteError> {
            if rows.key == ["PANIC"] {
                panic!("{WRITE_PANIC}");
            }
            if rows.key == ["BAD"] {
                return Err(WriteError {
                    message: "dividend 3 has no pay date".into(),
                });
            }
            Ok(rows.key.join("/").into_bytes())
        }
```

with, beside `UPLOAD_PANIC`:

```rust
    /// The message `KeyKind::write` panics with for a `PANIC` key.
    const WRITE_PANIC: &str = "the encoder fell over";
```

and the test:

```rust
    /// Encoding is foreign code on the worker now: its panic fails this
    /// upload alone, answered once, and the upload queued behind it still
    /// runs and answers.
    #[test]
    fn an_encoding_panic_answers_the_upload_and_keeps_the_worker() {
        let (entered_tx, entered) = sync_channel(64);
        let (release, release_rx) = sync_channel(64);
        let mut adapters = AdapterRegistry::default();
        adapters.register(Arc::new(TakeOnceAdapter {
            name: "gate",
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
        let release = release;
        let documents = documents();

        workers.upload(params(1, TARGET, DIVIDEND, "PANIC"), &documents);
        workers.upload(params(2, TARGET, DIVIDEND, "K1"), &documents);
        let first = next_upload(&rx);
        assert_eq!(first.tag, 1);
        let message = first.result.expect_err("an encoding panic is an error");
        assert!(
            message.contains("encoding panicked") && message.contains(WRITE_PANIC),
            "{message}"
        );
        assert!(message.contains(&format!("egress '{TARGET}'")), "{message}");
        assert_eq!(
            entered.recv_timeout(Duration::from_secs(5)).unwrap(),
            "gate/K1",
            "the queued upload behind it still reaches the transport"
        );
        release.send(()).unwrap();
        let second = next_upload(&rx);
        assert_eq!((second.tag, second.result), (2, Ok(())));
        assert_silent(&rx);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p geode-data --lib an_encoding_panic_answers_the_upload_and_keeps_the_worker`
Expected: FAIL — the panic unwinds `EgressWorkers::upload` in the test thread ("the encoder fell over").

- [ ] **Step 3: Implement**

Module doc (first paragraph and the second): "The service thread resolves the target, the address and the document kind, and hands the rows to that target's worker, so neither a slow encoder nor a slow transport blocks the request loop. … Validation and queue refusals answer on the service thread; encoding and transport results answer from the worker. An encoding or transport panic becomes an error naming the target and the step, and the worker continues with queued jobs. Neither call has a timeout, and the event sink can refuse an outcome."

Imports: `use geode_core::document::{DocumentKind, DocumentRows};` and `use std::sync::Arc;` (outside tests).

`Job`:

```rust
/// An accepted upload waiting for its target's worker, which encodes it and
/// sends it. The rows travel, not bytes: the encoder is foreign code and runs
/// inside the worker's boundary.
struct Job {
    key: QueryKey,
    tag: u64,
    document: String,
    document_key: String,
    address: String,
    rows: DocumentRows,
    kind: Arc<dyn DocumentKind>,
}
```

In `upload`, delete the `let bytes = match kind.write(&p.rows) { .. };` block and build the job with `rows: p.rows, kind,` in place of `bytes,` (the `refuse` closure captures only `p.target`, `p.document`, `p.key` and `p.tag`, so moving `p.rows` afterwards compiles).

Replace `work` with:

```rust
fn work(name: String, mut egress: Box<dyn Egress>, jobs: Receiver<Job>, sink: EventSink) {
    while let Ok(job) = jobs.recv() {
        let result = run_job(&name, egress.as_mut(), &job);
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

/// Encode and send one job. Both steps are foreign code (the document kind,
/// then the transport), so each runs inside its own marked boundary: a panic
/// in either fails this upload alone, named by step, and the worker goes on
/// to the next job. The caller answers exactly once with the result.
fn run_job(name: &str, egress: &mut dyn Egress, job: &Job) -> Result<(), String> {
    let bytes = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        geode_core::panic::contained(|| job.kind.write(&job.rows))
    })) {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(e)) => return Err(format!("egress '{name}': {e}")),
        Err(payload) => {
            return Err(format!(
                "egress '{name}': encoding panicked: {}",
                crate::ingest::runner::panic_payload_message(&*payload)
            ));
        }
    };
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        geode_core::panic::contained(|| egress.upload(&job.address, bytes))
    })) {
        Ok(outcome) => outcome.map_err(|e| format!("egress '{name}': {e}")),
        Err(payload) => Err(format!(
            "egress '{name}': transport panicked: {}",
            crate::ingest::runner::panic_payload_message(&*payload)
        )),
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p geode-data --lib egress` and `cargo test -p geode-data --lib an_upload_request_is_served_to_its_target_and_answered`
Expected: PASS, including the existing write-error, full-queue, order and transport-panic tests.

- [ ] **Step 5: Gate and commit**

Run: `cargo test -p geode-data`, `cargo clippy -p geode-data --all-targets -- -D warnings`, `cargo fmt --check`. (`--anchors-only` fails until Step 6 re-aims three entries; run it after Step 6.) If `docs/current/performance.md` names an upload path, note that encoding now runs on the target worker (read it with `grep -n "upload\|egress" docs/current/performance.md`; nothing to change if it names none).

```bash
git add crates/geode-data/src/egress.rs
git commit -m "feat(egress): encode on the target worker inside its own boundary

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

- [ ] **Step 6: Re-aim and add mutation entries**

Existing entries whose anchors this task changes (locate with `grep -n '"egress: a write error answers Err"\|"egress: a panicking transport is contained"\|"egress: uploads to one target run in submission order"' scripts/mutation-check.sh`), replace each whole entry with:

```
run_mutation "egress: a write error answers Err" \
  crates/geode-data/src/egress.rs \
  '        Ok(Err(e)) => return Err(format!("egress '"'"'{name}'"'"': {e}")),' \
  '        Ok(Err(_)) => Vec::new(),' \
  geode-data \
  a_write_error_an_unknown_target_and_a_closed_bus_each_answer_err_naming_the_target
```

```
# A transport panic must fail its own upload, not the worker. Mutated, it
# unwinds the worker, so the job is never answered and the queued uploads
# behind it are stranded.
run_mutation "egress: a panicking transport is contained" \
  crates/geode-data/src/egress.rs \
  '    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        geode_core::panic::contained(|| egress.upload(&job.address, bytes))
    })) {
        Ok(outcome) => outcome.map_err(|e| format!("egress '"'"'{name}'"'"': {e}")),
        Err(payload) => Err(format!(
            "egress '"'"'{name}'"'"': transport panicked: {}",
            crate::ingest::runner::panic_payload_message(&*payload)
        )),
    }' \
  '    egress
        .upload(&job.address, bytes)
        .map_err(|e| format!("egress '"'"'{name}'"'"': {e}"))' \
  geode-data \
  a_panicking_transport_answers_the_upload_and_keeps_the_worker
```

```
# One worker per target runs uploads in submission order: the later of two
# uploads of one document must land last. Mutated to run a second queued
# job ahead of the first, the order inverts.
run_mutation "egress: uploads to one target run in submission order" \
  crates/geode-data/src/egress.rs \
  '    while let Ok(job) = jobs.recv() {
        let result = run_job(&name, egress.as_mut(), &job);' \
  '    while let Ok(first) = jobs.recv() {
        std::thread::sleep(std::time::Duration::from_millis(50));
        let job = match jobs.try_recv() {
            Ok(second) => {
                let result = run_job(&name, egress.as_mut(), &second);
                answer(&sink, &name, &second.document, &second.document_key, second.key, second.tag, result);
                first
            }
            Err(_) => first,
        };
        let result = run_job(&name, egress.as_mut(), &job);' \
  geode-data \
  uploads_to_one_target_run_in_submission_order
```

New:

```
# An encoder panic must fail its own upload. Mutated, it unwinds the worker
# (now declared by supervision), the upload is never answered, and the one
# queued behind it is stranded.
run_mutation "egress: an encoding panic is contained" \
  crates/geode-data/src/egress.rs \
  '    let bytes = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        geode_core::panic::contained(|| job.kind.write(&job.rows))
    })) {' \
  '    let bytes = match Ok::<_, Box<dyn std::any::Any + Send>>(job.kind.write(&job.rows)) {' \
  geode-data an_encoding_panic_answers_the_upload_and_keeps_the_worker
```

Verify all four (`--build-check` and named run each), run `--anchors-only`, commit `test(mutation): egress encoding entries`.

---

### Task 4: Per-request containment in `serve`

**Files:**
- Modify: `crates/geode-data/src/handle.rs`, `crates/geode-data/src/service.rs`, `scripts/mutation-check.sh`
- Test: `crates/geode-data/src/handle.rs` tests (probed arms, loop death, guard, clean shutdown); `crates/geode-data/src/service.rs` tests (catalog and fetch production routes)

**Interfaces:**
- Consumes: `Refusal`, `stopped`, `spawn_supervised`, `REQUEST_LOOP` (Task 1).
- Produces:
  - private `enum ServePoint<'a> { Views, Arm(&'a Request), Loop(&'a Request) }`, `type Probe = fn(ServePoint<'_>)`, `fn no_probe(ServePoint<'_>)`.
  - `DataService::spawn_with_probe(config, sink, probe: Probe) -> DataHandle` (private; `spawn` calls it with `no_probe`).
  - `serve(config, sink, rx, pending_views, stopped, probe)`; private `fn dispatch(service: &DataService, sink: &EventSink, req: Request)`; private `enum PanicAnswer`, `fn error_diagnostic(message: String) -> Diagnostic`, `struct StoppedOnUnwind`.
  - `DataService::fail_fetch(&self, source: &str, identity: &str, reason: String)` (`pub(crate)`); new field `health: Arc<HealthTracker>`.
  - `DataService::replace_views` validates before it assigns.

- [ ] **Step 1: Write the failing probed-arm tests**

In `handle.rs` tests add the fixture and helpers:

```rust
    const ARM_PANIC: &str = "injected arm panic";
    const MARKED: QueryKey = QueryKey(666);

    fn is_marked(req: &Request) -> bool {
        match req {
            Request::Query(p) => p.key == MARKED,
            Request::Distinct(p) => p.key == MARKED,
            Request::Document(p) => p.key == MARKED,
            Request::Series(p) => p.key == MARKED,
            Request::Catalog(p) => p.key == MARKED,
            Request::Price(p) => p.key == MARKED,
            Request::Upload(p) => p.key == MARKED,
            Request::Fetch(p) => p.key == MARKED,
            Request::Publish(p) => p.dataset == "marked",
            Request::Forget(f) => f.dataset == "marked",
            Request::Identities { source } => source == "marked",
            Request::Cancel { key } => *key == MARKED,
            Request::ReplaceViews | Request::Shutdown => false,
        }
    }

    /// Panics inside a marked request's boundary: the arms no production
    /// input can panic.
    fn panic_marked_arms(point: ServePoint<'_>) {
        if let ServePoint::Arm(req) = point
            && is_marked(req)
        {
            panic!("{ARM_PANIC}");
        }
    }

    /// Panics outside every boundary on a marked cancel: loop death.
    fn panic_marked_cancel_outside(point: ServePoint<'_>) {
        if let ServePoint::Loop(Request::Cancel { key }) = point
            && *key == MARKED
        {
            panic!("injected loop panic");
        }
    }

    fn empty_config(dir: &std::path::Path) -> DataServiceConfig {
        DataServiceConfig {
            db_path: dir.join("geode.duckdb"),
            schema: geode_core::schema::SchemaSpec::default(),
            views: Vec::new(),
            dimensions: DerivedDimensions::default(),
            query_workers: 1,
            sources: Vec::new(),
            adapters: Default::default(),
            documents: Default::default(),
            egress: Vec::new(),
            pricer: PricerConfig::default(),
        }
    }

    fn probed(probe: Probe) -> (tempfile::TempDir, DataHandle, Receiver<DataEvent>) {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = channel();
        let sink: EventSink = Arc::new(move |e| tx.send(e).is_ok());
        let handle = DataService::spawn_with_probe(empty_config(dir.path()), sink, probe);
        (dir, handle, rx)
    }

    /// Everything the service emits before it answers a fresh catalog
    /// request: proof the loop still serves. A `ThreadStopped` fails.
    fn serves_on(handle: &DataHandle, rx: &Receiver<DataEvent>) -> Vec<DataEvent> {
        handle
            .catalog(CatalogParams {
                key: QueryKey(1),
                tag: 4242,
                as_of: AsOf::Live,
            })
            .unwrap();
        let mut seen = Vec::new();
        loop {
            match rx.recv_timeout(Duration::from_secs(30)).expect("the loop still answers") {
                DataEvent::Catalog(o) if o.tag == 4242 => {
                    assert!(o.snapshot.is_ok(), "{:?}", o.snapshot);
                    return seen;
                }
                DataEvent::ThreadStopped { thread, reason } => {
                    panic!("{thread} stopped: {reason}")
                }
                e => seen.push(e),
            }
        }
    }

    fn panicked(reason: &str, kind: &str) -> bool {
        reason.contains(&format!("{kind} request panicked")) && reason.contains(ARM_PANIC)
    }

    fn error_names(events: &[DataEvent], kind: &str) -> bool {
        events.iter().any(|e| {
            matches!(e, DataEvent::Diagnostics(d)
                if d.iter().any(|d| d.severity == Severity::Error && panicked(&d.message, kind)))
        })
    }
```

Then one test per probed arm (each: submit the marked request, then `serves_on`, and assert the answer):

```rust
    #[test]
    fn a_panicking_query_is_answered_on_its_key_and_the_loop_serves_on() {
        let (_d, h, rx) = probed(panic_marked_arms);
        h.query(params(MARKED.0, "tree")).unwrap();
        let seen = serves_on(&h, &rx);
        assert!(seen.iter().any(|e| matches!(e, DataEvent::Query(o)
            if o.key == MARKED && o.snapshot.as_ref().is_err_and(|r| panicked(r, "query")))), "{seen:?}");
    }

    #[test]
    fn a_panicking_document_request_is_answered_on_its_key() {
        let (_d, h, rx) = probed(panic_marked_arms);
        h.document(DocumentParams {
            key: MARKED,
            tag: 1,
            submitted: Instant::now(),
            dataset: "cvi_params".into(),
            document_key: vec!["SPX.Z".into()],
            as_of: AsOf::Live,
        })
        .unwrap();
        let seen = serves_on(&h, &rx);
        assert!(seen.iter().any(|e| matches!(e, DataEvent::Query(o)
            if o.key == MARKED && o.snapshot.as_ref().is_err_and(|r| panicked(r, "document")))), "{seen:?}");
    }

    #[test]
    fn a_panicking_distinct_request_is_answered_on_its_key_and_column() {
        let (_d, h, rx) = probed(panic_marked_arms);
        h.distinct(distinct_params(MARKED.0, "book")).unwrap();
        let seen = serves_on(&h, &rx);
        assert!(seen.iter().any(|e| matches!(e, DataEvent::Distinct(o)
            if o.key == MARKED && o.column == "book"
                && o.values.as_ref().is_err_and(|r| panicked(r, "distinct")))), "{seen:?}");
    }

    #[test]
    fn a_panicking_series_request_is_answered_on_its_key() {
        let (_d, h, rx) = probed(panic_marked_arms);
        h.series(series_params(MARKED.0)).unwrap();
        let seen = serves_on(&h, &rx);
        assert!(seen.iter().any(|e| matches!(e, DataEvent::Series(o)
            if o.key == MARKED && o.result.as_ref().is_err_and(|r| panicked(r, "series")))), "{seen:?}");
    }

    #[test]
    fn a_panicking_catalog_request_is_answered_on_its_key() {
        let (_d, h, rx) = probed(panic_marked_arms);
        h.catalog(CatalogParams { key: MARKED, tag: 7, as_of: AsOf::Live }).unwrap();
        let seen = serves_on(&h, &rx);
        assert!(seen.iter().any(|e| matches!(e, DataEvent::Catalog(o)
            if o.key == MARKED && o.tag == 7
                && o.snapshot.as_ref().is_err_and(|r| panicked(r, "catalog")))), "{seen:?}");
    }

    #[test]
    fn a_panicking_price_request_answers_every_line() {
        let (_d, h, rx) = probed(panic_marked_arms);
        h.price(crate::pricing::worker::tests::params(MARKED.0, 3, &["SPX", "NDX"])).unwrap();
        let seen = serves_on(&h, &rx);
        assert!(seen.iter().any(|e| matches!(e, DataEvent::Price(o)
            if o.key == MARKED && o.tag == 3 && o.results.len() == 2
                && o.results.iter().all(|(_, _, r)| r.as_ref().is_err_and(|r| panicked(r, "price"))))), "{seen:?}");
    }

    #[test]
    fn a_panicking_upload_is_answered_on_its_key_and_target() {
        let (_d, h, rx) = probed(panic_marked_arms);
        let mut upload = upload_params(5);
        upload.key = MARKED;
        h.upload(upload).unwrap();
        let seen = serves_on(&h, &rx);
        assert!(seen.iter().any(|e| matches!(e, DataEvent::Upload(o)
            if o.key == MARKED && o.tag == 5 && o.target == "sophis"
                && o.result.as_ref().is_err_and(|r| panicked(r, "upload")))), "{seen:?}");
    }

    #[test]
    fn a_panicking_publish_is_a_diagnostic_and_its_writers_failure() {
        let (_d, h, rx) = probed(panic_marked_arms);
        h.publish(LocalPublish { dataset: "marked".into(), rows: sheet_rows("s", &[1]) }).unwrap();
        let seen = serves_on(&h, &rx);
        assert!(error_names(&seen, "publish"), "{seen:?}");
        assert!(seen.iter().any(|e| matches!(e, DataEvent::LocalPublishFailed { dataset, batch, reason }
            if dataset == "marked" && batch == "s" && panicked(reason, "publish"))), "{seen:?}");
    }

    #[test]
    fn a_panicking_forget_is_a_diagnostic_and_its_askers_failure() {
        let (_d, h, rx) = probed(panic_marked_arms);
        h.forget(crate::service::LocalForget { dataset: "marked".into(), key: vec!["s".into()] }).unwrap();
        let seen = serves_on(&h, &rx);
        assert!(error_names(&seen, "forget"), "{seen:?}");
        assert!(seen.iter().any(|e| matches!(e, DataEvent::ForgetFailed { dataset, batch, reason }
            if dataset == "marked" && batch == "s" && panicked(reason, "forget"))), "{seen:?}");
    }

    #[test]
    fn a_panicking_identities_request_is_one_error_diagnostic() {
        let (_d, h, rx) = probed(panic_marked_arms);
        h.identities("marked").unwrap();
        let seen = serves_on(&h, &rx);
        assert!(error_names(&seen, "identities"), "{seen:?}");
    }

    #[test]
    fn a_panicking_cancel_is_one_error_diagnostic() {
        let (_d, h, rx) = probed(panic_marked_arms);
        assert!(h.cancel(MARKED));
        let seen = serves_on(&h, &rx);
        assert!(error_names(&seen, "cancel"), "{seen:?}");
    }

    #[test]
    fn a_panicking_view_replacement_keeps_the_previous_views_and_serves_on() {
        fn panic_views(point: ServePoint<'_>) {
            if let ServePoint::Views = point {
                panic!("injected view panic");
            }
        }
        let (_d, h, rx) = probed(panic_views);
        h.replace_views(Vec::new(), DerivedDimensions::default()).unwrap();
        let seen = serves_on(&h, &rx);
        assert!(seen.iter().any(|e| matches!(e, DataEvent::Diagnostics(d) if d.iter().any(|d|
            d.severity == Severity::Error
                && d.message.contains("view replacement panicked")
                && d.message.contains("injected view panic")
                && d.message.contains("previous views stay in force")))), "{seen:?}");
    }

    #[test]
    fn a_panic_outside_every_arm_declares_the_request_loop_stopped() {
        let (_d, h, rx) = probed(panic_marked_cancel_outside);
        assert!(h.cancel(MARKED));
        let (thread, reason) = crate::supervise::tests_support::next_stop(&rx);
        assert_eq!(thread, crate::supervise::REQUEST_LOOP);
        assert!(reason.contains("injected loop panic"), "{reason}");
        assert_eq!(h.query(params(1, "tree")), Err(Refusal::Stopped));
        assert_eq!(h.dropped_requests(), 0, "a stopped loop is not a busy one");
    }

    #[test]
    fn a_clean_shutdown_declares_nothing() {
        let (_d, h, rx) = probed(no_probe);
        let _ = serves_on(&h, &rx);
        h.shutdown();
        assert!(
            rx.try_iter().all(|e| !matches!(e, DataEvent::ThreadStopped { .. })),
            "a quit is not a failure"
        );
        assert!(!h.inner.stopped.load(Ordering::Acquire));
    }

    /// The dying loop joins its workers before its receiver drops; a
    /// submission in that window must be refused `Stopped`, not admitted to
    /// a queue nothing will read. A pricer line held for three seconds keeps
    /// the window open.
    #[test]
    fn a_submission_while_the_dying_loop_joins_its_workers_is_refused_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let asked: Arc<Mutex<Vec<String>>> = Default::default();
        let mut config = empty_config(dir.path());
        config.pricer = PricerConfig::with(Arc::new(crate::pricing::worker::tests::FakePricer {
            asked: Arc::clone(&asked),
            delay: Duration::from_secs(3),
            overrides_seen: Default::default(),
        }));
        let (tx, rx) = channel();
        let sink: EventSink = Arc::new(move |e| tx.send(e).is_ok());
        let h = DataService::spawn_with_probe(config, sink, panic_marked_cancel_outside);
        h.price(crate::pricing::worker::tests::params(5, 1, &["SPX"])).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        while asked.lock().unwrap().is_empty() {
            assert!(Instant::now() < deadline, "the pricer never started its line");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(h.cancel(MARKED));
        let sent = Instant::now();
        while h.query(params(1, "tree")) != Err(Refusal::Stopped) {
            assert!(
                sent.elapsed() < Duration::from_secs(2),
                "the dying loop admitted submissions while it joined its workers"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            rx.try_iter().all(|e| !matches!(e, DataEvent::ThreadStopped { .. })),
            "refused Stopped while the loop was still dying, before it was declared"
        );
    }
```

(`FakePricer`'s field types are those `pricing::worker::tests::worker` builds it with; if `asked` holds something other than `Vec<String>`, use that type.)

**Controller ruling — no wall-clock windows.** The sketch above holds the window open with a 3 s pricer delay against a 2 s deadline, and asserts that no `ThreadStopped` has arrived yet. Under load that races. Implement it deterministically instead: a test pricer whose `price` blocks on a `std::sync::mpsc::Receiver<()>` the test holds (or a `Barrier`), so the dying loop cannot finish joining its workers until the test releases it. While the pricer is held, poll `h.query(..)` until it returns `Err(Refusal::Stopped)`. The poll's generous deadline (e.g. 30 s) is only a hang guard, not the contract. Assert no `ThreadStopped` has been received, then release the pricer and assert exactly one `ThreadStopped` arrives. No assertion may depend on how long anything takes.

In `service.rs` tests add the production-route tests:

```rust
    /// A series dataset whose coverage row holds a timestamp past chrono's
    /// range: `from_micros` panics reading it, in the catalog and in a fetch.
    fn out_of_range_coverage_service() -> (
        tempfile::TempDir,
        crate::handle::DataHandle,
        std::sync::mpsc::Receiver<DataEvent>,
    ) {
        const PAST_CHRONO: i64 = 9_000_000_000_000_000_000;
        assert!(chrono::DateTime::<Utc>::from_timestamp_micros(PAST_CHRONO).is_none());
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        let ds = crate::store::ddl::tests_support::series_dataset();
        store.apply_schema(&ds).unwrap();
        Catalog::new(store.writer()).ensure_tables().unwrap();
        store
            .writer()
            .execute_batch(&format!(
                "insert into series_series_coverage values \
                 ('kdb_hist', 'SPX', make_timestamp({PAST_CHRONO}::BIGINT), \
                  make_timestamp({PAST_CHRONO}::BIGINT), now()::timestamp);"
            ))
            .unwrap();
        drop(store);
        let mut schema = SchemaSpec::default();
        schema.datasets.push(ds);
        let mut adapters = AdapterRegistry::default();
        adapters.register(Arc::new(FakeFetchAdapter {
            calls: Default::default(),
            catalogue: None,
            fail_once: false,
        }));
        let (tx, rx) = std::sync::mpsc::channel();
        let sink: EventSink = Arc::new(move |e| tx.send(e).is_ok());
        let handle = DataService::spawn(
            DataServiceConfig {
                db_path: dir.path().join("geode.duckdb"),
                schema,
                views: Vec::new(),
                dimensions: DerivedDimensions::default(),
                query_workers: 1,
                sources: vec![crate::source::SourceSpec {
                    adapter: "fake_kdb".to_string(),
                    ..crate::source::SourceSpec::directory("kdb_hist", "series", Vec::new())
                }],
                adapters,
                documents: Default::default(),
                egress: Vec::new(),
                pricer: PricerConfig::default(),
            },
            sink,
        );
        (dir, handle, rx)
    }

    /// An unknown view answers at once: proof the loop is still serving.
    fn still_serves(handle: &crate::handle::DataHandle, rx: &std::sync::mpsc::Receiver<DataEvent>) {
        handle.query(params(4242, "nonesuch", &Scope::default(), AsOf::Live, 1)).unwrap();
        until(rx, |e| match e {
            DataEvent::Query(o) if o.key == QueryKey(4242) => Some(()),
            DataEvent::ThreadStopped { thread, reason } => panic!("{thread} stopped: {reason}"),
            _ => None,
        });
    }

    #[test]
    fn a_catalog_over_an_out_of_range_timestamp_answers_err_and_the_loop_serves_on() {
        let (_d, handle, rx) = out_of_range_coverage_service();
        handle.catalog(CatalogParams { key: QueryKey(3), tag: 9, as_of: AsOf::Live }).unwrap();
        let answer = until(&rx, |e| match e {
            DataEvent::Catalog(o) if o.tag == 9 => Some(o),
            _ => None,
        });
        let reason = answer.snapshot.expect_err("a panicking catalog read is an error");
        assert!(reason.contains("catalog request panicked"), "{reason}");
        assert!(reason.contains("a stored timestamp is in range"), "{reason}");
        still_serves(&handle, &rx);
        handle.shutdown();
    }

    #[test]
    fn a_fetch_over_an_out_of_range_timestamp_fails_the_pair_and_the_loop_serves_on() {
        let (_d, handle, rx) = out_of_range_coverage_service();
        let now = Utc::now();
        handle
            .fetch(FetchParams {
                key: QueryKey(3),
                source: "kdb_hist".into(),
                identity: "SPX".into(),
                from: now - chrono::Duration::days(1),
                to: now,
            })
            .unwrap();
        let health = until(&rx, |e| match e {
            DataEvent::Health { source, worst: Health::Failed { reason }, detail } if source == "kdb_hist" => Some((reason, detail)),
            _ => None,
        });
        assert!(health.0.contains("fetch request panicked"), "{health:?}");
        assert!(health.1.starts_with("SPX@kdb_hist"), "{health:?}");
        let fetched = until(&rx, |e| match e {
            DataEvent::SeriesFetched { source, identity, result } if source == "kdb_hist" && identity == "SPX" => Some(result),
            _ => None,
        });
        assert!(fetched.is_err_and(|r| r.contains("a stored timestamp is in range")));
        still_serves(&handle, &rx);
        handle.shutdown();
    }
```

If DuckDB refuses `make_timestamp(BIGINT)` for this value, insert the literal `'287000-01-01 00:00:00'::TIMESTAMP` instead; the `from_timestamp_micros` guard stays as proof the stored value is past chrono.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-data --lib panicking` and `cargo test -p geode-data --lib out_of_range_timestamp`
Expected: FAIL to compile (`spawn_with_probe`, `ServePoint`); after Step 3's scaffolding alone the arm tests fail with `the loop still answers` timeouts or `ThreadStopped` panics.

- [ ] **Step 3: Implement**

`handle.rs` — add:

```rust
/// Where `serve` calls its probe. Production passes [`no_probe`]; a test
/// passes one that panics at a chosen point, for the arms and steps that no
/// production input can panic.
#[derive(Clone, Copy)]
#[cfg_attr(not(test), allow(dead_code))]
enum ServePoint<'a> {
    /// Inside the view-replacement boundary, before the replacement runs.
    Views,
    /// Inside a request's boundary, before its arm runs.
    Arm(&'a Request),
    /// Outside every boundary, just after a request is received.
    Loop(&'a Request),
}

type Probe = fn(ServePoint<'_>);

fn no_probe(_: ServePoint<'_>) {}

fn error_diagnostic(message: String) -> Diagnostic {
    Diagnostic {
        severity: Severity::Error,
        layer: None,
        file: None,
        message,
        path: None,
    }
}

/// Declares the request loop stopped when it is dropped by an unwind.
/// Declared after the service in `serve`, so it drops first: the flag is set
/// before the dying loop joins its workers, which can take as long as the
/// slowest running job, and a submission in that window is refused
/// `Stopped` rather than admitted to a queue nothing will read.
struct StoppedOnUnwind(Arc<AtomicBool>);

impl Drop for StoppedOnUnwind {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.store(true, Ordering::Release);
        }
    }
}

/// Who a request's answer goes to, taken before its arm runs, so a panicking
/// arm is still answered exactly once through the door that answers its
/// success. A tile never waits on a request the loop swallowed.
enum PanicAnswer {
    Query { key: QueryKey, tag: u64, submitted: Instant },
    Distinct { key: QueryKey, tag: u64, column: String },
    Series { key: QueryKey, tag: u64, submitted: Instant },
    Catalog { key: QueryKey, tag: u64 },
    Price { key: QueryKey, tag: u64, submitted: Instant, lines: Vec<(u64, u64)> },
    Upload { key: QueryKey, tag: u64, target: String },
    Fetch { source: String, identity: String },
    Publish { dataset: String, batch: String },
    Forget { dataset: String, batch: String },
    /// Identities, cancel and view replacement have no answer path.
    Unanswered,
}

impl PanicAnswer {
    /// The request's kind, for the message, and its answer.
    fn of(req: &Request) -> (&'static str, PanicAnswer) {
        match req {
            Request::Query(p) => ("query", PanicAnswer::Query { key: p.key, tag: p.tag, submitted: p.submitted }),
            Request::Document(p) => ("document", PanicAnswer::Query { key: p.key, tag: p.tag, submitted: p.submitted }),
            Request::Distinct(p) => ("distinct", PanicAnswer::Distinct { key: p.key, tag: p.tag, column: p.column.clone() }),
            Request::Series(p) => ("series", PanicAnswer::Series { key: p.key, tag: p.tag, submitted: p.submitted }),
            Request::Catalog(p) => ("catalog", PanicAnswer::Catalog { key: p.key, tag: p.tag }),
            Request::Price(p) => (
                "price",
                PanicAnswer::Price {
                    key: p.key,
                    tag: p.tag,
                    submitted: p.submitted,
                    lines: p.lines.iter().map(|l| (l.id, l.revision)).collect(),
                },
            ),
            Request::Upload(p) => ("upload", PanicAnswer::Upload { key: p.key, tag: p.tag, target: p.target.clone() }),
            Request::Fetch(p) => ("fetch", PanicAnswer::Fetch { source: p.source.clone(), identity: p.identity.clone() }),
            Request::Publish(p) => (
                "publish",
                PanicAnswer::Publish { dataset: p.dataset.clone(), batch: geode_core::document::join_key(&p.rows.key) },
            ),
            Request::Forget(f) => (
                "forget",
                PanicAnswer::Forget { dataset: f.dataset.clone(), batch: geode_core::document::join_key(&f.key) },
            ),
            Request::Identities { .. } => ("identities", PanicAnswer::Unanswered),
            Request::Cancel { .. } => ("cancel", PanicAnswer::Unanswered),
            Request::ReplaceViews => ("view replacement", PanicAnswer::Unanswered),
            Request::Shutdown => ("shutdown", PanicAnswer::Unanswered),
        }
    }

    fn answer(self, service: &DataService, sink: &EventSink, reason: String) {
        match self {
            PanicAnswer::Query { key, tag, submitted } => {
                let _ = sink(DataEvent::Query(QueryOutcome {
                    key,
                    tag,
                    snapshot: Err(reason),
                    submitted,
                }));
            }
            PanicAnswer::Distinct { key, tag, column } => {
                let _ = sink(DataEvent::Distinct(DistinctOutcome {
                    key,
                    tag,
                    column,
                    values: Err(reason),
                }));
            }
            PanicAnswer::Series { key, tag, submitted } => {
                let _ = sink(DataEvent::Series(SeriesOutcome {
                    key,
                    tag,
                    submitted,
                    result: Err(reason),
                }));
            }
            PanicAnswer::Catalog { key, tag } => {
                let _ = sink(DataEvent::Catalog(CatalogOutcome {
                    key,
                    tag,
                    snapshot: Err(reason),
                }));
            }
            PanicAnswer::Price { key, tag, submitted, lines } => {
                let results = lines
                    .into_iter()
                    .map(|(id, revision)| (id, revision, Err(reason.clone())))
                    .collect();
                let _ = sink(DataEvent::Price(PriceOutcome {
                    key,
                    tag,
                    submitted,
                    results,
                }));
            }
            PanicAnswer::Upload { key, tag, target } => {
                let _ = sink(DataEvent::Upload(UploadOutcome {
                    key,
                    tag,
                    target,
                    result: Err(reason),
                }));
            }
            PanicAnswer::Fetch { source, identity } => service.fail_fetch(&source, &identity, reason),
            PanicAnswer::Publish { dataset, batch } => {
                let _ = sink(DataEvent::Diagnostics(vec![error_diagnostic(format!(
                    "local publish of {dataset}/{batch} failed: {reason}"
                ))]));
                let _ = sink(DataEvent::LocalPublishFailed {
                    dataset,
                    batch,
                    reason,
                });
            }
            PanicAnswer::Forget { dataset, batch } => {
                let _ = sink(DataEvent::Diagnostics(vec![error_diagnostic(format!(
                    "forgetting {dataset}/{batch} failed: {reason}"
                ))]));
                let _ = sink(DataEvent::ForgetFailed {
                    dataset,
                    batch,
                    reason,
                });
            }
            PanicAnswer::Unanswered => {
                let _ = sink(DataEvent::Diagnostics(vec![error_diagnostic(reason)]));
            }
        }
    }
}
```

Imports: `use crate::egress::{UploadOutcome, UploadParams};`, `use geode_core::pricing::{LocalPublish, PriceOutcome, PriceParams};`, `use geode_core::query::{CatalogOutcome, CatalogParams, DistinctOutcome, DistinctParams, DocumentParams, QueryKey, QueryOutcome};`, `use std::time::Instant;` (as the compiler asks). In Task 1's open-failure arm, `Diagnostic { .. }` may now be written `error_diagnostic(reason.clone())`.

`DataService::spawn` becomes `Self::spawn_with_probe(config, sink, no_probe)`; `spawn_with_probe` is Task 1's body with `probe` passed to `serve`.

`serve`, after the diagnostics-at-open block:

```rust
    let _declare = StoppedOnUnwind(Arc::clone(&stopped));
    while let Ok(req) = rx.recv() {
        probe(ServePoint::Loop(&req));
        // Check before every request: a full request queue is already a wakeup.
        // The latest configuration therefore cannot be lost behind a burst.
        let replacement = pending_views
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(ViewReplacement { views, dimensions }) = replacement {
            // Contained like a request: a panic keeps the previous views (the
            // replacement validates before it assigns) and the loop serves on.
            let replaced = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                geode_core::panic::contained(|| {
                    probe(ServePoint::Views);
                    service.replace_views(views, dimensions)
                })
            }));
            let diags = replaced.unwrap_or_else(|payload| {
                vec![error_diagnostic(format!(
                    "view replacement panicked: {}; the previous views stay in force",
                    crate::ingest::runner::panic_payload_message(payload.as_ref())
                ))]
            });
            if !diags.is_empty() {
                let _ = sink(DataEvent::Diagnostics(diags));
            }
        }
        if matches!(req, Request::Shutdown) {
            break;
        }
        // One request's panic is that request's error, answered once through
        // its own door; the next request is still served.
        let (kind, answer) = PanicAnswer::of(&req);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            geode_core::panic::contained(|| {
                probe(ServePoint::Arm(&req));
                dispatch(&service, &sink, req);
            })
        }));
        if let Err(payload) = outcome {
            let payload = crate::ingest::runner::panic_payload_message(payload.as_ref());
            tracing::error!(target: "geode::ingest", "a {kind} request panicked on the request loop: {payload}");
            answer.answer(&service, &sink, format!("{kind} request panicked: {payload}"));
        }
    }
    service.shutdown();
```

Move the old `match req { .. }` into `fn dispatch(service: &DataService, sink: &EventSink, req: Request)` unchanged except `sink(..)` becomes `let _ = sink(..)` where it was a statement, and the last arm becomes `Request::ReplaceViews | Request::Shutdown => {}` with the comment "Both are handled in `serve` before dispatch."

`serve` signature gains `probe: Probe`; the test at `view_reload_survives_a_full_request_queue_and_keeps_the_latest` passes `no_probe`.

`service.rs`:
- `DataService` gains a field after `sink`:
  ```rust
    /// The source-health lanes, shared with every worker sink, so a fetch
    /// the request loop could not run is failed on the same load lane the
    /// fetch worker reports on.
    health: Arc<HealthTracker>,
  ```
  set in `open` to `Arc::clone(&health_tracker)`.
- `replace_views` validates first:
  ```rust
        // Validate before assigning anything: if validation panics, the
        // previous views, dimensions and refusals stay in force together.
        let (diagnostics, refused_views) =
            validate_views(&views, &self.config.schema, &dimensions);
        self.read_config = Arc::new(ReadConfig {
            schema: Arc::clone(&self.read_config.schema),
            dimensions: dimensions.clone(),
        });
        self.config.dimensions = dimensions;
        self.config.views = views;
        self.diagnostics = diagnostics.clone();
        self.refused_views = refused_views;
        diagnostics
  ```
- `fail_fetch`:
  ```rust
    /// Answer a fetch the request loop could not run the way the fetch worker
    /// answers its own panic: the pair's load lane goes `Failed`, then
    /// `SeriesFetched` carries the error, so the asking tile and every other
    /// tile watching the pair hear back.
    pub(crate) fn fail_fetch(&self, source: &str, identity: &str, reason: String) {
        let pair = format!("{identity}@{source}");
        self.health.report_load_and_emit(
            source,
            &pair,
            Health::Failed {
                reason: reason.clone(),
            },
            format!("{pair}: {reason}"),
            |reported| match reported {
                Some((worst, detail)) => {
                    log_health_event(source, &worst, &detail);
                    (self.sink)(DataEvent::Health {
                        source: source.to_string(),
                        worst,
                        detail,
                    })
                }
                None => true,
            },
        );
        let _ = (self.sink)(DataEvent::SeriesFetched {
            source: source.to_string(),
            identity: identity.to_string(),
            result: Err(reason),
        });
    }
  ```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p geode-data --lib panicking`, `cargo test -p geode-data --lib out_of_range_timestamp`, `cargo test -p geode-data --lib a_clean_shutdown_declares_nothing`, `cargo test -p geode-data --lib a_submission_while_the_dying_loop`
Expected: PASS.

- [ ] **Step 5: Gate and commit**

Run: `cargo test -p geode-data`, `cargo clippy -p geode-data --all-targets -- -D warnings`, `cargo fmt --check`. (`--anchors-only` after Step 6.)

```bash
git add crates/geode-data
git commit -m "feat(data): every request-loop arm is contained and answered once

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

- [ ] **Step 6: Mutation entries**

Re-aim `handle: a compile failure is delivered as the key's outcome` (its arm moved into `dispatch`):

```
run_mutation "handle: a compile failure is delivered as the key's outcome" \
  crates/geode-data/src/handle.rs \
  '            if let Err(e) = service.query(&params) {' \
  '            if let Err(e) = service.query(&params) && false {' \
  geode-data \
  the_real_service_answers_through_the_sink_and_reports_open_failures
```

New (each replacement turns the answer's send into a discarded tuple, so the arm is swallowed):

```
# A panicking arm must not end the loop: every later submission would be
# admitted and never served.
run_mutation "serve: a panicking arm ends the loop" \
  crates/geode-data/src/handle.rs \
  '        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            geode_core::panic::contained(|| {
                probe(ServePoint::Arm(&req));
                dispatch(&service, &sink, req);
            })
        }));' \
  '        probe(ServePoint::Arm(&req));
        dispatch(&service, &sink, req);
        let outcome: std::thread::Result<()> = Ok(());' \
  geode-data a_panicking_query_is_answered_on_its_key_and_the_loop_serves_on

run_mutation "serve: a panicking query is swallowed" \
  crates/geode-data/src/handle.rs \
  '                let _ = sink(DataEvent::Query(QueryOutcome {' \
  '                let _ = (sink, DataEvent::Query(QueryOutcome {' \
  geode-data a_panicking_query_is_answered_on_its_key_and_the_loop_serves_on

run_mutation "serve: a panicking document request is answered as another kind" \
  crates/geode-data/src/handle.rs \
  '            Request::Document(p) => ("document", PanicAnswer::Query { key: p.key, tag: p.tag, submitted: p.submitted }),' \
  '            Request::Document(_) => ("document", PanicAnswer::Unanswered),' \
  geode-data a_panicking_document_request_is_answered_on_its_key

run_mutation "serve: a panicking distinct request is swallowed" \
  crates/geode-data/src/handle.rs \
  '                let _ = sink(DataEvent::Distinct(DistinctOutcome {' \
  '                let _ = (sink, DataEvent::Distinct(DistinctOutcome {' \
  geode-data a_panicking_distinct_request_is_answered_on_its_key_and_column

run_mutation "serve: a panicking series request is swallowed" \
  crates/geode-data/src/handle.rs \
  '                let _ = sink(DataEvent::Series(SeriesOutcome {' \
  '                let _ = (sink, DataEvent::Series(SeriesOutcome {' \
  geode-data a_panicking_series_request_is_answered_on_its_key

run_mutation "serve: a panicking catalog request is swallowed" \
  crates/geode-data/src/handle.rs \
  '                let _ = sink(DataEvent::Catalog(CatalogOutcome {' \
  '                let _ = (sink, DataEvent::Catalog(CatalogOutcome {' \
  geode-data a_panicking_catalog_request_is_answered_on_its_key

run_mutation "serve: a panicking price request is swallowed" \
  crates/geode-data/src/handle.rs \
  '                let _ = sink(DataEvent::Price(PriceOutcome {' \
  '                let _ = (sink, DataEvent::Price(PriceOutcome {' \
  geode-data a_panicking_price_request_answers_every_line

run_mutation "serve: a panicking upload is swallowed" \
  crates/geode-data/src/handle.rs \
  '                let _ = sink(DataEvent::Upload(UploadOutcome {' \
  '                let _ = (sink, DataEvent::Upload(UploadOutcome {' \
  geode-data a_panicking_upload_is_answered_on_its_key_and_target

run_mutation "serve: a panicking publish leaves its writer waiting" \
  crates/geode-data/src/handle.rs \
  '                let _ = sink(DataEvent::LocalPublishFailed {' \
  '                let _ = (sink, DataEvent::LocalPublishFailed {' \
  geode-data a_panicking_publish_is_a_diagnostic_and_its_writers_failure

run_mutation "serve: a panicking forget leaves its asker waiting" \
  crates/geode-data/src/handle.rs \
  '                let _ = sink(DataEvent::ForgetFailed {' \
  '                let _ = (sink, DataEvent::ForgetFailed {' \
  geode-data a_panicking_forget_is_a_diagnostic_and_its_askers_failure

run_mutation "serve: a panic with no answer path is silent" \
  crates/geode-data/src/handle.rs \
  '                let _ = sink(DataEvent::Diagnostics(vec![error_diagnostic(reason)]));' \
  '                let _ = (sink, reason);' \
  geode-data a_panicking_identities_request_is_one_error_diagnostic

run_mutation "serve: a panicking view replacement ends the loop" \
  crates/geode-data/src/handle.rs \
  '            let replaced = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                geode_core::panic::contained(|| {
                    probe(ServePoint::Views);
                    service.replace_views(views, dimensions)
                })
            }));' \
  '            probe(ServePoint::Views);
            let replaced: std::thread::Result<Vec<Diagnostic>> =
                Ok(service.replace_views(views, dimensions));' \
  geode-data a_panicking_view_replacement_keeps_the_previous_views_and_serves_on

run_mutation "serve: a dying loop still admits submissions" \
  crates/geode-data/src/handle.rs \
  '            self.0.store(true, Ordering::Release);' \
  '            let _ = &self.0;' \
  geode-data a_submission_while_the_dying_loop_joins_its_workers_is_refused_stopped

run_mutation "serve: a panicking fetch leaves the pair unfailed" \
  crates/geode-data/src/handle.rs \
  '            PanicAnswer::Fetch { source, identity } => service.fail_fetch(&source, &identity, reason),' \
  '            PanicAnswer::Fetch { .. } => {}' \
  geode-data a_fetch_over_an_out_of_range_timestamp_fails_the_pair_and_the_loop_serves_on

run_mutation "service: a failed fetch skips the load lane" \
  crates/geode-data/src/service.rs \
  '        self.health.report_load_and_emit(
            source,
            &pair,' \
  '        let _ = &self.health;
        HealthTracker::default().report_load_and_emit(
            source,
            &pair,' \
  geode-data a_fetch_over_an_out_of_range_timestamp_fails_the_pair_and_the_loop_serves_on
```

`a_clean_shutdown_declares_nothing` guards the `std::thread::panicking()` condition: add

```
run_mutation "serve: a clean quit is declared stopped" \
  crates/geode-data/src/handle.rs \
  '        if std::thread::panicking() {' \
  '        if true {' \
  geode-data a_clean_shutdown_declares_nothing
```

Verify all, `--anchors-only`, commit `test(mutation): request-loop containment entries`.

---

### Task 5: The five no-data panics

**Files:**
- Modify: `crates/geode-data/src/ingest/fetch.rs`, `ingest/runner.rs`, `ingest/scheduler.rs`, `service.rs`, `scripts/mutation-check.sh`
- Test: the same modules

**Interfaces:**
- Consumes: `panic_payload_message`, `unwatched` (Tasks 1–2).
- Produces:
  - `FetchOutcome::IdentitiesPanicked(String)` (the payload).
  - `IngestEvent::Diagnostic(geode_core::config::Diagnostic)`; `service.rs` maps it to `DataEvent::Diagnostics(vec![d])`.
  - runner: `fn sweep_local(store, dataset, batch, sink: &IngestSink, refusal_logged: &AtomicBool) -> bool`, `fn sweep_local_with(.., body: SweepFn) -> bool`, `type SweepFn = fn(&Store, &DatasetSpec, &str) -> Result<bool, String>`, `fn sweep_body(..)`, `fn report_stale_check(sink, refusal_logged, path, what)`.
  - service: `fn result_event(r: QueryResult, health: &HealthTracker) -> DataEvent`, `fn contained_result_event(r: QueryResult, build: impl FnOnce(QueryResult) -> DataEvent) -> DataEvent`, `fn identity_listing_panicked(source: &str, payload: &str) -> DataEvent`.

- [ ] **Step 1: Write the failing tests**

`ingest/fetch.rs` tests:

```rust
    struct PanickingCatalogue;
    impl Fetch for PanickingCatalogue {
        fn fetch(&mut self, _: &FetchRequest) -> Result<SeriesRows, AdapterError> {
            unreachable!("only the catalogue is asked")
        }
        fn catalogue(&mut self) -> Option<Vec<String>> {
            panic!("the listing fell over")
        }
    }

    #[test]
    fn a_panicking_identity_listing_is_an_outcome_not_a_log_line() {
        let (tx, rx) = channel();
        let sink: FetchOutcomeSink = Arc::new(move |o| {
            let _ = tx.send(o);
        });
        let mut w = FetchWorker::spawn(
            "demo_kdb",
            Box::new(PanickingCatalogue),
            sink,
            crate::supervise::unwatched(),
        )
        .unwrap();
        assert!(w.request(FetchWork::Identities));
        match rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap() {
            FetchOutcome::IdentitiesPanicked(payload) => {
                assert!(payload.contains("the listing fell over"), "{payload}")
            }
            other => panic!("{other:?}"),
        }
        w.shutdown();
    }
```

`service.rs` tests:

```rust
    struct PanickingCatalogueAdapter;
    impl crate::adapter::Adapter for PanickingCatalogueAdapter {
        fn name(&self) -> &'static str {
            "fake_kdb"
        }
        fn subscription(&self) -> Option<Box<dyn crate::adapter::Subscription>> {
            None
        }
        fn egress(&self) -> Option<Box<dyn crate::adapter::Egress>> {
            None
        }
        fn fetch(&self) -> Option<Box<dyn crate::adapter::Fetch>> {
            struct Listing;
            impl crate::adapter::Fetch for Listing {
                fn fetch(
                    &mut self,
                    _: &crate::adapter::FetchRequest,
                ) -> Result<crate::adapter::SeriesRows, crate::adapter::AdapterError> {
                    unreachable!("only the catalogue is asked")
                }
                fn catalogue(&mut self) -> Option<Vec<String>> {
                    panic!("the listing fell over")
                }
            }
            Some(Box::new(Listing))
        }
    }

    /// Nobody asked for the listing (open asks), so the only door is an
    /// error diagnostic naming the source and the payload.
    #[test]
    fn a_panicking_identity_listing_is_an_error_diagnostic_naming_the_source() {
        let dir = tempfile::tempdir().unwrap();
        let mut adapters = AdapterRegistry::default();
        adapters.register(Arc::new(PanickingCatalogueAdapter));
        let mut schema = SchemaSpec::default();
        schema.datasets.push(crate::store::ddl::tests_support::series_dataset());
        let (_service, rx) = DataService::open_channel(DataServiceConfig {
            db_path: dir.path().join("geode.duckdb"),
            schema,
            views: Vec::new(),
            dimensions: DerivedDimensions::default(),
            query_workers: 1,
            sources: vec![crate::source::SourceSpec {
                adapter: "fake_kdb".to_string(),
                ..crate::source::SourceSpec::directory("kdb_hist", "series", Vec::new())
            }],
            adapters,
            documents: Default::default(),
            egress: Vec::new(),
            pricer: PricerConfig::default(),
        })
        .unwrap();
        let message = until(&rx, |e| match e {
            DataEvent::Diagnostics(d) => d
                .into_iter()
                .find(|d| d.severity == Severity::Error && d.message.contains("identity listing")),
            _ => None,
        });
        assert!(
            message.message.contains("identity listing for kdb_hist panicked: the listing fell over"),
            "{message:?}"
        );
    }

    /// The stale check fails open (a failed lookup must not discard the
    /// load) and is reported: a warning naming the file reaches the sink.
    #[test]
    fn a_failed_stale_check_is_a_warning_diagnostic_through_the_service() {
        let (db, src, store, ds, _emitted) = crate::ingest::load::tests_support::fixture();
        let spec = crate::source::SourceSpec {
            pending_timeout: Duration::from_secs(3600),
            batch_pattern: Some(r"^risk_\d{4}-\d{2}-\d{2}_(?<batch>.+)$".into()),
            ..crate::source::SourceSpec::directory(
                "risk",
                "risk_snapshot",
                vec![format!("{}/*.csv", src.path().display())],
            )
        };
        let found = crate::source::discover(&spec, &Catalog::new(store.writer()), SystemTime::now()).unwrap();
        let plan = crate::ingest::build_plan(&[(spec, found)]);
        let poisoned = plan.items[0].clone();
        // A NULL mtime makes the catalog lookup's row read panic.
        store
            .writer()
            .execute_batch(&format!(
                "insert into file_generations
                     (file_id, dataset, batch, path, size, mtime, source_time,
                      gen_id, loaded_at, row_count, health, health_reason,
                      archived_only)
                 values
                     (-1, '{}', '{}', '{}', {}, NULL, '{}'::timestamptz, -1,
                      now(), 1, 'ok', NULL, false);",
                poisoned.dataset,
                poisoned.batch,
                poisoned.candidate.csv_path.display(),
                poisoned.candidate.size,
                poisoned.source_time.to_rfc3339(),
            ))
            .unwrap();
        drop(store);
        let mut schema = SchemaSpec::default();
        schema.datasets.push(ds);
        let (service, rx) = DataService::open_channel(DataServiceConfig {
            db_path: db.path().join("geode.duckdb"),
            schema,
            views: Vec::new(),
            dimensions: DerivedDimensions::default(),
            query_workers: 1,
            sources: Vec::new(),
            adapters: Default::default(),
            documents: Default::default(),
            egress: Vec::new(),
            pricer: PricerConfig::default(),
        })
        .unwrap();
        service.ingest.submit(crate::ingest::WorkPlan { items: vec![poisoned.clone()] });
        let warning = until(&rx, |e| match e {
            DataEvent::Diagnostics(d) => d.into_iter().find(|d| d.severity == Severity::Warning),
            _ => None,
        });
        assert!(
            warning.message.contains(&poisoned.candidate.csv_path.display().to_string())
                && warning.message.contains("panicked"),
            "{warning:?}"
        );
        service.shutdown();
    }

    #[test]
    fn a_distinct_answer_without_its_columns_is_an_err_for_its_key() {
        let empty = geode_core::snapshot::Snapshot::from_batches(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            geode_core::snapshot::Provenance::default(),
        )
        .unwrap();
        let event = result_event(
            crate::query::pool::QueryResult {
                id: 1,
                key: QueryKey(3),
                tag: 2,
                submitted: Instant::now(),
                view: crate::query::pool::ViewId("distinct".into()),
                payload: Ok(Payload::Snapshot(empty)),
                kind: RequestKind::Distinct { column: "book".into() },
            },
            &HealthTracker::default(),
        );
        match event {
            DataEvent::Distinct(o) => {
                assert_eq!((o.key, o.tag, o.column.as_str()), (QueryKey(3), 2, "book"));
                assert!(o.values.is_err_and(|e| e.contains("value")), "an Err for the key, not a panic");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_panic_building_a_result_event_answers_its_key_with_an_error() {
        let event = contained_result_event(
            crate::query::pool::QueryResult {
                id: 1,
                key: QueryKey(4),
                tag: 7,
                submitted: Instant::now(),
                view: crate::query::pool::ViewId("v".into()),
                payload: Err("unused".into()),
                kind: RequestKind::Query,
            },
            |_| panic!("the event builder fell over"),
        );
        match event {
            DataEvent::Query(o) => {
                assert_eq!((o.key, o.tag), (QueryKey(4), 7));
                let reason = o.snapshot.expect_err("a panicking build is an error");
                assert!(reason.contains("the event builder fell over"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
    }
```

`ingest/runner.rs` tests — in `a_malformed_catalog_row_panics_the_pop_time_lookup_without_killing_the_runner`, after the two existing assertions add:

```rust
        let path = poisoned.candidate.csv_path.display().to_string();
        assert!(
            events.iter().any(|e| matches!(e, IngestEvent::Diagnostic(d)
                if d.severity == geode_core::config::Severity::Warning
                    && d.message.contains(&path)
                    && d.message.contains("panicked"))),
            "the fail-open is reported, naming the file and the payload: {events:?}"
        );
```

and add:

```rust
    #[test]
    fn a_panicking_local_sweep_is_an_error_diagnostic() {
        use crate::store::ddl::tests_support::local_dataset;
        let (_dir, _path, store, _schema) = local_store();
        let ds = local_dataset();
        let (tx, rx) = channel();
        let sink: IngestSink = Arc::new(move |e| tx.send(e).is_ok());
        let latch = AtomicBool::new(false);
        assert!(!sweep_local_with(&store, &ds, "s", &sink, &latch, |_, _, _| {
            panic!("the sweep fell over")
        }));
        match rx.try_recv().expect("the panic is reported") {
            IngestEvent::Diagnostic(d) => {
                assert_eq!(d.severity, geode_core::config::Severity::Error);
                assert!(d.message.contains("local sweep panicked: the sweep fell over"), "{d:?}");
            }
            other => panic!("{other:?}"),
        }
    }
```

Update `a_local_sweep_runs_only_past_the_bound_and_prunes_provenance`'s two `sweep_local(&store, &ds, "s")` calls to `sweep_local(&store, &ds, "s", &quiet, &latch)` with `let quiet: IngestSink = Arc::new(|_| true); let latch = AtomicBool::new(false);`, and add `IngestEvent::Diagnostic(_) => "diagnostic",` to the exhaustive `kinds` match in `started_precedes_each_publish_and_counts_what_is_still_queued`.

`ingest/scheduler.rs` tests:

```rust
    #[test]
    fn a_panicking_discovery_poll_names_its_payload() {
        let (_db, dir, ingest, _ingest_rx, conn, spec, _ds) =
            harness(Duration::from_millis(50), Duration::from_secs(3600));
        let csv = dir.path().join("risk_2026-08-24_BK0.csv");
        std::fs::write(&csv, "Book\nBK0\n").unwrap();
        std::fs::write(
            dir.path().join("risk_2026-08-24_BK0.csv.done"),
            r#"{"as_of":"2026-08-24T07:00:00Z","columns":["Book"],"books":["BK0"]}"#,
        )
        .unwrap();
        // A NULL mtime makes the catalog lookup's row read panic.
        conn.execute_batch(&format!(
            "insert into file_generations
                 (file_id, dataset, batch, path, size, mtime, source_time,
                  gen_id, loaded_at, row_count, health, health_reason, archived_only)
             values
                 (-1, 'risk_snapshot', 'BK0', '{}', 1, NULL,
                  '2026-08-24T07:00:00Z'::timestamptz, -1, now(), 1, 'ok', NULL, false);",
            csv.display()
        ))
        .unwrap();
        let (sink, sched_rx) = events_sink();
        let sched = Scheduler::spawn(vec![spec], conn, ingest, sink, crate::supervise::unwatched());
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let reason = loop {
            assert!(std::time::Instant::now() < deadline, "no failed health");
            if let Ok(SchedulerEvent::Health { worst: Health::Failed { reason }, .. }) =
                sched_rx.recv_timeout(Duration::from_secs(1))
            {
                break reason;
            }
        };
        assert!(
            reason.starts_with("discovery panicked: ") && reason.len() > "discovery panicked: ".len(),
            "the payload rides the health reason: {reason}"
        );
        sched.shutdown();
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-data --lib identity_listing`, `cargo test -p geode-data --lib stale_check`, `cargo test -p geode-data --lib a_malformed_catalog_row`, `cargo test -p geode-data --lib a_panicking_local_sweep`, `cargo test -p geode-data --lib a_panicking_discovery_poll`, `cargo test -p geode-data --lib result_event`, `cargo test -p geode-data --lib without_its_columns`
Expected: FAIL to compile (new variants/functions).

- [ ] **Step 3: Implement**

`fetch.rs`:
- variant, after `Identities(..)`:
  ```rust
    /// The identity listing panicked; the payload. No tile asked for it, so
    /// the service reports it as an error diagnostic naming the source.
    IdentitiesPanicked(String),
  ```
- the panic arm in `run`:
  ```rust
            Err(payload) => {
                let message = crate::ingest::runner::panic_payload_message(payload.as_ref());
                tracing::error!(target: "geode::ingest", "a fetch panicked: {message}");
                if let Some(identity) = identity {
                    sink(FetchOutcome::Failed {
                        identity,
                        reason: format!("fetch panicked: {message}"),
                    });
                } else {
                    sink(FetchOutcome::IdentitiesPanicked(message));
                }
            }
  ```

`service.rs`:
- module-level:
  ```rust
/// A fetch source's identity listing panicked. Nobody asked for it, so the
/// only door is an error diagnostic naming the source and the payload.
fn identity_listing_panicked(source: &str, payload: &str) -> DataEvent {
    DataEvent::Diagnostics(vec![Diagnostic {
        severity: Severity::Error,
        layer: None,
        file: None,
        message: format!("identity listing for {source} panicked: {payload}"),
        path: None,
    }])
}
  ```
- in the fetch `outcome_sink`, capture `let sink = Arc::clone(&sink);` (already there) and add the arm:
  ```rust
                            FetchOutcome::IdentitiesPanicked(payload) => {
                                let _ = sink(identity_listing_panicked(&source, &payload));
                            }
  ```
- in `ingest_sink`, before `IngestEvent::PlanComplete`:
  ```rust
                // A condition the runner reports without failing a job: a
                // stale check that could not read the catalog, a local sweep
                // that panicked.
                IngestEvent::Diagnostic(d) => sink(DataEvent::Diagnostics(vec![d])),
  ```
- result events. Move the body of the `result_sink` closure into:
  ```rust
/// The event a pool result becomes. A payload of the wrong kind, or a
/// distinct snapshot missing its `value`/`n` columns, is that key's error,
/// never a panic: this runs on a query worker.
fn result_event(r: QueryResult, health_tracker: &HealthTracker) -> DataEvent {
    match r.kind {
        RequestKind::Query => DataEvent::Query(QueryOutcome {
            key: r.key,
            tag: r.tag,
            snapshot: r.payload.and_then(view_snapshot).map(Arc::new),
            submitted: r.submitted,
        }),
        RequestKind::Distinct { column } => DataEvent::Distinct(DistinctOutcome {
            key: r.key,
            tag: r.tag,
            column,
            values: r.payload.and_then(view_snapshot).and_then(|s| {
                let (Some(v), Some(n)) = (s.column_index("value"), s.column_index("n")) else {
                    return Err("internal: a distinct answer without its value and n columns".to_string());
                };
                Ok((0..s.rows())
                    .filter_map(|row| {
                        Some((s.text_at(v, row)?.to_string(), s.i64_at(n, row)? as u64))
                    })
                    .collect())
            }),
        }),
        // Match health to source slots by slot number. Expression slots leave gaps,
        // so positional zipping would attach health to the wrong result.
        RequestKind::Series { pairs } => {
            let result = match r.payload {
                Ok(Payload::Series(mut res)) => {
                    for (slot, source, identity) in &pairs {
                        let key = format!("{identity}@{source}");
                        if let Some(s) = res.slots.iter_mut().find(|s| s.slot == *slot) {
                            s.provenance.health = health_tracker.load_lane(source, &key);
                        }
                    }
                    Ok(res)
                }
                // Report a mismatched worker payload as an error rather than panicking.
                Ok(Payload::Snapshot(_)) => {
                    Err("internal: a series request answered with a snapshot".to_string())
                }
                Err(e) => Err(e),
            };
            DataEvent::Series(SeriesOutcome {
                key: r.key,
                tag: r.tag,
                submitted: r.submitted,
                result,
            })
        }
    }
}

/// Build `r`'s event inside a panic boundary. The pool worker sends what
/// this returns outside its own boundary, so a panic here would end the
/// worker; instead the key is answered with an error of its own kind.
fn contained_result_event(
    r: QueryResult,
    build: impl FnOnce(QueryResult) -> DataEvent,
) -> DataEvent {
    let (key, tag, submitted, kind) = (r.key, r.tag, r.submitted, r.kind.clone());
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        geode_core::panic::contained(|| build(r))
    })) {
        Ok(event) => event,
        Err(payload) => {
            let reason = format!(
                "result delivery panicked: {}",
                crate::ingest::runner::panic_payload_message(payload.as_ref())
            );
            match kind {
                RequestKind::Query => DataEvent::Query(QueryOutcome {
                    key,
                    tag,
                    snapshot: Err(reason),
                    submitted,
                }),
                RequestKind::Distinct { column } => DataEvent::Distinct(DistinctOutcome {
                    key,
                    tag,
                    column,
                    values: Err(reason),
                }),
                RequestKind::Series { .. } => DataEvent::Series(SeriesOutcome {
                    key,
                    tag,
                    submitted,
                    result: Err(reason),
                }),
            }
        }
    }
}
  ```
  and the closure in `open` becomes:
  ```rust
        let result_sink: ResultSink = {
            let sink = Arc::clone(&sink);
            // The tracker rides into the sink so a series result can carry
            // each pair's load-lane word without a second trip through the
            // service thread.
            let health_tracker = Arc::clone(&health_tracker);
            Arc::new(move |r: QueryResult| {
                sink(contained_result_event(r, |r| result_event(r, &health_tracker)))
            })
        };
  ```

`runner.rs`:
- variant, before `PlanComplete`:
  ```rust
    /// A condition to report that fails no job: a pop-time stale check that
    /// could not read the catalog (the load proceeds), a local sweep that
    /// panicked (the save stands).
    Diagnostic(geode_core::config::Diagnostic),
  ```
- imports: `use geode_core::config::{Diagnostic, Severity};`, `use geode_core::schema::{DatasetSpec, SchemaSpec};`.
- the stale check (replacing the `let stale = … .unwrap_or(false);` expression):
  ```rust
        // Recheck the catalog before loading: work may have become redundant while
        // queued. A stale skip emits no event. Lookup errors and contained panics
        // fail open, deliberately: a failed lookup must not discard the load, and
        // the cost is a redundant reload of the same rows as a new generation,
        // never a wrong total. Each is reported, so an unreadable catalog is not
        // silent.
        let lookup = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            geode_core::panic::contained(|| {
                Catalog::new(store.writer()).lookup_by_path(&item.candidate.csv_path)
            })
        }));
        let stale = match lookup {
            Ok(Ok(Some(prev))) => is_unchanged(&prev, item.candidate.size, item.source_time),
            Ok(Ok(None)) => false,
            Ok(Err(e)) => {
                report_stale_check(&sink, &refusal_logged, &item.candidate.csv_path, &e.to_string());
                false
            }
            Err(payload) => {
                let what = format!("panicked: {}", panic_payload_message(payload.as_ref()));
                report_stale_check(&sink, &refusal_logged, &item.candidate.csv_path, &what);
                false
            }
        };
  ```
  and the helper:
  ```rust
/// Report a pop-time stale check that could not decide. The load proceeds
/// (fail open); this warning is the only trace that it went unchecked.
fn report_stale_check(sink: &IngestSink, refusal_logged: &AtomicBool, path: &std::path::Path, what: &str) {
    let delivered = sink(IngestEvent::Diagnostic(Diagnostic {
        severity: Severity::Warning,
        layer: None,
        file: None,
        message: format!(
            "the stale check for {} could not read the catalog ({what}); loading it anyway",
            path.display()
        ),
        path: None,
    }));
    if !delivered {
        log_refused_event(refusal_logged, "a stale-check warning");
    }
}
  ```
- sweep:
  ```rust
/// The sweep work behind [`sweep_local`], injectable so a test can panic it:
/// no stored state makes the real sweep panic.
type SweepFn = fn(&Store, &DatasetSpec, &str) -> Result<bool, String>;

fn sweep_body(store: &Store, dataset: &DatasetSpec, batch: &str) -> Result<bool, String> {
    let policy = RetentionPolicy {
        keep_generations: Some(LOCAL_KEEP_GENERATIONS),
        keep_age: None,
    };
    let pairs = crate::store::ddl::table_pairs(dataset);
    if !local_needs_sweep(store, dataset, batch).map_err(|e| e.to_string())? {
        return Ok(false);
    }
    sweep(store.writer(), dataset, &pairs, &policy, Utc::now()).map_err(|e| e.to_string())?;
    prune_orphan_provenance(store, dataset).map_err(|e| e.to_string())?;
    Ok(true)
}

fn sweep_local(store: &Store, dataset: &DatasetSpec, batch: &str, sink: &IngestSink, refusal_logged: &AtomicBool) -> bool {
    sweep_local_with(store, dataset, batch, sink, refusal_logged, sweep_body)
}

fn sweep_local_with(
    store: &Store,
    dataset: &DatasetSpec,
    batch: &str,
    sink: &IngestSink,
    refusal_logged: &AtomicBool,
    body: SweepFn,
) -> bool {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        geode_core::panic::contained(|| body(store, dataset, batch))
    }));
    let reason = match outcome {
        Ok(Ok(swept)) => return swept,
        Ok(Err(reason)) => reason,
        Err(payload) => {
            let payload = panic_payload_message(payload.as_ref());
            // A panic here is a defect in the sweep, not a full disk: say so
            // where a trader looks, not only in the log.
            let delivered = sink(IngestEvent::Diagnostic(Diagnostic {
                severity: Severity::Error,
                layer: None,
                file: None,
                message: format!("local sweep panicked: {payload}"),
                path: None,
            }));
            if !delivered {
                log_refused_event(refusal_logged, "a local-sweep panic");
            }
            format!("panicked: {payload}")
        }
    };
    tracing::warn!(
        target: "geode::ingest",
        "retention sweep of local dataset '{}' failed (history is kept until the next publish sweeps): {reason}",
        dataset.name,
    );
    false
}
  ```
  Keep `sweep_local`'s existing doc comment on `sweep_local`. In `publish_one_document`, the call becomes `sweep_local(store, dataset, &batch, sink, refusal_logged);`.

`scheduler.rs` failure arm:

```rust
            Err(payload) => {
                let reason = format!(
                    "discovery panicked: {}",
                    crate::ingest::runner::panic_payload_message(payload.as_ref())
                );
                Refused::health(!sink(SchedulerEvent::Health {
                    source: spec.name.clone(),
                    worst: Health::Failed {
                        reason: reason.clone(),
                    },
                    detail: reason,
                }))
            }
```

- [ ] **Step 4: Run tests to verify they pass**

Run the Step 2 commands; Expected: PASS.

- [ ] **Step 5: Gate and commit**

Run: `cargo test -p geode-data`, `cargo clippy -p geode-data --all-targets -- -D warnings`, `cargo fmt --check`.

```bash
git add crates/geode-data
git commit -m "feat(data): the five contained panics are reported, not dropped

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

- [ ] **Step 6: Mutation entries**

Re-aim the two result-sink entries (their lines lost `sink(` and moved to `result_event`, 8-space arms):

```
run_mutation "service: an outcome carries the caller's key" \
  crates/geode-data/src/service.rs \
  '        RequestKind::Query => DataEvent::Query(QueryOutcome {
            key: r.key,' \
  '        RequestKind::Query => DataEvent::Query(QueryOutcome {
            key: QueryKey(0),' \
  geode-data \
  an_outcome_is_addressed_to_the_key_that_asked
```

```
run_mutation "distinct: the sink maps a Distinct result to a Distinct event" \
  crates/geode-data/src/service.rs \
  '            values: r.payload.and_then(view_snapshot).and_then(|s| {
                let (Some(v), Some(n)) = (s.column_index("value"), s.column_index("n")) else {
                    return Err("internal: a distinct answer without its value and n columns".to_string());
                };
                Ok((0..s.rows())
                    .filter_map(|row| {
                        Some((s.text_at(v, row)?.to_string(), s.i64_at(n, row)? as u64))
                    })
                    .collect())
            }),' \
  '            values: Ok(Vec::new()),' \
  geode-data a_distinct_query_returns_value_counts_on_the_distinct_event
```

(Copy the block from the formatted source; rustfmt may wrap the `return Err(..)` line.)

The two series-health entries anchored lines that are now 8 spaces shallower in `result_event`; re-aim both:

```
run_mutation "series query: the pair's health is not attached" \
  crates/geode-data/src/service.rs \
  '                            s.provenance.health = health_tracker.load_lane(source, &key);' \
  '                            let _ = (&key, &health_tracker, &mut s.provenance);' \
  geode-data \
  a_failed_pairs_health_rides_its_slot

run_mutation "series query: health is filed by position, not slot" \
  crates/geode-data/src/service.rs \
  '                    for (slot, source, identity) in &pairs {
                        let key = format!("{identity}@{source}");
                        if let Some(s) = res.slots.iter_mut().find(|s| s.slot == *slot) {' \
  '                    for (i, (_slot, source, identity)) in pairs.iter().enumerate() {
                        let key = format!("{identity}@{source}");
                        if let Some(s) = res.slots.iter_mut().nth(i) {' \
  geode-data \
  health_is_attached_by_slot_number_not_position
```

(Take the exact indentation from the formatted source.) The fetch entry `consistency: fetch panics deliver failures` keeps its anchor (`                if let Some(identity) = identity {` is unchanged).

New:

```
run_mutation "service: a distinct answer without its columns panics" \
  crates/geode-data/src/service.rs \
  '                let (Some(v), Some(n)) = (s.column_index("value"), s.column_index("n")) else {' \
  '                let (Some(v), Some(n)) = (Some(s.column_index("value").expect("value")), Some(s.column_index("n").expect("n"))) else {' \
  geode-data a_distinct_answer_without_its_columns_is_an_err_for_its_key

run_mutation "service: a panic building a result event ends its worker" \
  crates/geode-data/src/service.rs \
  '    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        geode_core::panic::contained(|| build(r))
    })) {' \
  '    match Ok::<_, Box<dyn std::any::Any + Send>>(build(r)) {' \
  geode-data a_panic_building_a_result_event_answers_its_key_with_an_error

run_mutation "fetch: a panicking identity listing is only logged" \
  crates/geode-data/src/ingest/fetch.rs \
  '                    sink(FetchOutcome::IdentitiesPanicked(message));' \
  '                    let _ = message;' \
  geode-data a_panicking_identity_listing_is_an_outcome_not_a_log_line

run_mutation "service: a panicking identity listing reaches no one" \
  crates/geode-data/src/service.rs \
  '                                let _ = sink(identity_listing_panicked(&source, &payload));' \
  '                                let _ = (&sink, &source, &payload);' \
  geode-data a_panicking_identity_listing_is_an_error_diagnostic_naming_the_source

run_mutation "runner: a failed stale check is silent" \
  crates/geode-data/src/ingest/runner.rs \
  '    let delivered = sink(IngestEvent::Diagnostic(Diagnostic {
        severity: Severity::Warning,' \
  '    let delivered = true || sink(IngestEvent::Diagnostic(Diagnostic {
        severity: Severity::Warning,' \
  geode-data a_malformed_catalog_row_panics_the_pop_time_lookup_without_killing_the_runner

run_mutation "service: a runner diagnostic reaches no one" \
  crates/geode-data/src/service.rs \
  '                IngestEvent::Diagnostic(d) => sink(DataEvent::Diagnostics(vec![d])),' \
  '                IngestEvent::Diagnostic(_) => true,' \
  geode-data a_failed_stale_check_is_a_warning_diagnostic_through_the_service

run_mutation "runner: a panicking local sweep is only logged" \
  crates/geode-data/src/ingest/runner.rs \
  '            let delivered = sink(IngestEvent::Diagnostic(Diagnostic {
                severity: Severity::Error,' \
  '            let delivered = true || sink(IngestEvent::Diagnostic(Diagnostic {
                severity: Severity::Error,' \
  geode-data a_panicking_local_sweep_is_an_error_diagnostic

run_mutation "scheduler: a discovery panic drops its payload" \
  crates/geode-data/src/ingest/scheduler.rs \
  '                    "discovery panicked: {}",
                    crate::ingest::runner::panic_payload_message(payload.as_ref())' \
  '                    "discovery panicked{}",
                    { let _ = payload; "" }' \
  geode-data a_panicking_discovery_poll_names_its_payload
```

Verify each; `--anchors-only`; commit `test(mutation): no-data panic entries`.

---

### Task 6: Stopped and refused in the shell, the status bar and the diagnostics tile

**Files:**
- Modify: `crates/geode-shell/src/diagnostics.rs`, `crates/geode-shell/src/shell/status.rs`, `crates/geode-shell/src/shell/render.rs`, `crates/geode-diagnostics/src/sections.rs`, `crates/geode-app/src/bridge.rs`, `scripts/mutation-check.sh`
- Test: `crates/geode-shell/src/diagnostics.rs` tests, `crates/geode-shell/src/shell/tests/diagnostics.rs`, `crates/geode-diagnostics/src/sections.rs` tests, `crates/geode-app/src/bridge.rs` tests

**Interfaces:**
- Consumes: `DataEvent::ThreadStopped` (Task 1), `DataHandle::dropped_requests` (Busy only, Task 1), `fill_for_tests`.
- Produces:
  - `pub struct geode_shell::diagnostics::StoppedThread { pub thread: String, pub label: String, pub reason: String, pub at: SystemTime }`
  - `pub struct StoppedSegment { pub text: SharedString, pub detail: SharedString }`
  - `pub fn thread_label(thread: &str) -> String`
  - `Diagnostics { pub stopped: Vec<StoppedThread>, pub refused: u64, .. }`, `note_thread_stopped(&mut self, thread: &str, reason: String, at: SystemTime)`, `note_refused(&mut self, total: u64)`, `stopped_segment(&self) -> Option<&StoppedSegment>`.
  - `status::status_bar(.., stopped: Option<&StoppedSegment>, diagnostics_summary, on_diagnostics_click: impl Fn(&mut Window, &mut App) + Clone + 'static, ..)`.

- [ ] **Step 1: Write the failing pure tests** (`geode-shell/src/diagnostics.rs` tests)

```rust
    #[test]
    fn one_stopped_thread_is_named_with_its_reason_in_the_tooltip() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.note_thread_stopped("geode-ingest", "boom".into(), SystemTime::UNIX_EPOCH);
        let seg = d.stopped_segment().expect("a segment");
        assert_eq!(seg.text.as_ref(), "ingest stopped");
        assert_eq!(seg.detail.as_ref(), "boom");
        assert_eq!(d.stopped[0].label, "ingest");
    }

    #[test]
    fn two_stopped_threads_collapse_to_a_count_listing_each() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.note_thread_stopped("geode-discovery", "a".into(), SystemTime::UNIX_EPOCH);
        d.note_thread_stopped("geode-query-2", "b".into(), SystemTime::UNIX_EPOCH);
        let seg = d.stopped_segment().unwrap();
        assert_eq!(seg.text.as_ref(), "2 data threads stopped");
        assert_eq!(seg.detail.as_ref(), "discovery: a; query worker 2: b");
    }

    #[test]
    fn a_stopped_request_loop_outranks_the_other_threads() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.note_thread_stopped("geode-ingest", "a".into(), SystemTime::UNIX_EPOCH);
        d.note_thread_stopped("geode-data", "the loop died".into(), SystemTime::UNIX_EPOCH);
        let seg = d.stopped_segment().unwrap();
        assert_eq!(seg.text.as_ref(), "data service stopped — restart Geode");
        assert_eq!(seg.detail.as_ref(), "the loop died; also stopped: ingest");
    }

    #[test]
    fn a_thread_reported_twice_is_recorded_once() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.note_thread_stopped("geode-ingest", "boom".into(), SystemTime::UNIX_EPOCH);
        let version = d.version();
        d.note_thread_stopped("geode-ingest", "boom".into(), SystemTime::UNIX_EPOCH);
        assert_eq!(d.stopped.len(), 1);
        assert_eq!(d.version(), version, "a repeat changes nothing");
    }

    #[test]
    fn refused_submissions_show_in_the_summary_and_are_omitted_at_zero() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.note_refused(0);
        assert_eq!(d.summary().as_ref(), "");
        d.note_refused(3);
        d.note_dropped(2);
        assert_eq!(d.summary().as_ref(), "2 dropped · 3 refused");
    }

    #[test]
    fn thread_labels_are_readable() {
        for (thread, label) in [
            ("geode-data", "data service"),
            ("geode-ingest", "ingest"),
            ("geode-discovery", "discovery"),
            ("geode-pricing", "pricing"),
            ("geode-query-0", "query worker 0"),
            ("geode-fetch-kdb", "fetch kdb"),
            ("geode-subscribe-cvi", "subscription cvi"),
            ("geode-egress-sophis", "egress sophis"),
            ("something-else", "something-else"),
        ] {
            assert_eq!(thread_label(thread), label);
        }
    }
```

`geode-diagnostics/src/sections.rs` tests:

```rust
    #[test]
    fn stopped_threads_lead_the_sources_section() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.note_health("risk", Health::Ok, String::new(), SystemTime::UNIX_EPOCH);
        let at = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(3_600);
        d.note_thread_stopped("geode-ingest", "boom".into(), at);
        let rows = sources_rows(&d, at, Clock::utc());
        assert_eq!(rows[0].tone, Tone::Error);
        assert!(rows[0].text.contains("stopped threads"), "{:?}", rows[0].text);
        assert_eq!(rows[1].depth, 1);
        assert_eq!(
            rows[1].text.as_ref(),
            format!("ingest: boom (at {})", local_hms(at, Clock::utc()))
        );
        assert!(rows[2].text.starts_with("risk"), "sources follow");
    }
```

`geode-shell/src/shell/tests/diagnostics.rs`:

```rust
#[gpui::test]
fn a_stopped_thread_paints_the_stopped_segment_first(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut cx);
    let diagnostics = shell.read_with(&cx, |s, _| s.diagnostics().clone());
    assert!(cx.debug_bounds("data-stopped").is_none());
    diagnostics.update(&mut cx, |d, cx| {
        d.note_health("risk", Health::Degraded { reason: "x".into() }, "x".into(), SystemTime::now());
        d.note_thread_stopped("geode-ingest", "boom".into(), SystemTime::now());
        cx.notify();
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let stopped = cx.debug_bounds("data-stopped").expect("the stopped segment is painted");
    let summary = cx.debug_bounds("diagnostics-summary").expect("the summary is painted");
    assert!(stopped.origin.x < summary.origin.x, "the stopped segment leads");
}

#[gpui::test]
fn clicking_the_stopped_segment_opens_the_diagnostics_tile(cx: &mut gpui::TestAppContext) {
    let (mut services, _log) = services_with_recorder();
    services
        .roster
        .add(Box::new(crate::module::recording::RecordingFactory::new("diagnostics")));
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    let diagnostics = shell.read_with(&cx, |s, _| s.diagnostics().clone());
    diagnostics.update(&mut cx, |d, cx| {
        d.note_thread_stopped("geode-data", "boom".into(), SystemTime::now());
        cx.notify();
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let bounds = cx.debug_bounds("data-stopped").unwrap();
    cx.simulate_mouse_down(bounds.center(), gpui::MouseButton::Left, gpui::Modifiers::default());
    cx.simulate_mouse_up(bounds.center(), gpui::MouseButton::Left, gpui::Modifiers::default());
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let tile = shell.read_with(&cx, |s, _| s.services.workspaces.active().focused_tile());
    assert_eq!(
        shell.read_with(&cx, |s, _| s.occupant_kind(tile.expect("a tile opened"))),
        Some("diagnostics")
    );
}
```

`geode-app/src/bridge.rs` tests (use `catalog_fixture`, whose `events` sender reaches the drain):

```rust
    #[gpui::test]
    fn a_thread_stopped_event_reaches_the_status_segment(cx: &mut gpui::TestAppContext) {
        let f = catalog_fixture(cx);
        let mut vcx = gpui::VisualTestContext::from_window(f.window.into(), cx);
        let shell = f.window.root(&mut vcx).unwrap().read_with(&vcx, |r, _| {
            r.view().clone().downcast::<ShellView>().unwrap()
        });
        let diagnostics = shell.read_with(&vcx, |s, _| s.diagnostics().clone());
        f.events
            .try_send(DataEvent::ThreadStopped {
                thread: "geode-ingest".into(),
                reason: "boom".into(),
            })
            .unwrap();
        vcx.run_until_parked();
        assert_eq!(
            diagnostics.read_with(&vcx, |d, _| d.stopped_segment().map(|s| s.text.to_string())),
            Some("ingest stopped".to_string())
        );
    }

    #[gpui::test]
    fn refused_submissions_reach_the_status_summary(cx: &mut gpui::TestAppContext) {
        let f = catalog_fixture(cx);
        let mut vcx = gpui::VisualTestContext::from_window(f.window.into(), cx);
        let shell = f.window.root(&mut vcx).unwrap().read_with(&vcx, |r, _| {
            r.view().clone().downcast::<ShellView>().unwrap()
        });
        let diagnostics = shell.read_with(&vcx, |s, _| s.diagnostics().clone());
        f.bridge.handle.fill_for_tests();
        assert_eq!(f.bridge.handle.query(geode_data::QueryParams {
            key: QueryKey(1),
            tag: 1,
            submitted: std::time::Instant::now(),
            view: "tree".into(),
            grouping: None,
            scope: Default::default(),
            as_of: AsOf::Live,
            max_depth: 1,
        }), Err(geode_data::Refusal::Busy));
        let refused = f.bridge.handle.dropped_requests();
        assert!(refused > 0);
        f.events.try_send(DataEvent::LoadEnded).unwrap();
        vcx.run_until_parked();
        assert_eq!(diagnostics.read_with(&vcx, |d, _| d.refused), refused);
        assert!(diagnostics.read_with(&vcx, |d, _| d.summary().contains(&format!("{refused} refused"))));
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-shell --lib stopped`, `cargo test -p geode-shell --lib refused_submissions`, `cargo test -p geode-diagnostics --lib stopped_threads`, `cargo test -p geode-app --lib a_thread_stopped_event`, `cargo test -p geode-app --lib refused_submissions_reach`
Expected: FAIL to compile.

- [ ] **Step 3: Implement**

`geode-shell/src/diagnostics.rs` (add `use gpui::SharedString;`):

```rust
/// The request loop's thread name as the data layer spawns it. The shell
/// cannot depend on the data crate, so it is repeated here.
const REQUEST_LOOP: &str = "geode-data";

/// A data thread that died despite containment. It stays dead until the app
/// restarts, so nothing clears it.
#[derive(Debug, Clone, PartialEq)]
pub struct StoppedThread {
    /// The spawn name the data layer reported.
    pub thread: String,
    /// What the status bar and the diagnostics tile call it.
    pub label: String,
    pub reason: String,
    pub at: SystemTime,
}

/// The status bar's stopped segment, prepared when a thread stops so paint
/// formats nothing.
#[derive(Debug, Clone, PartialEq)]
pub struct StoppedSegment {
    pub text: SharedString,
    pub detail: SharedString,
}

/// A readable name for a data thread's spawn name.
pub fn thread_label(thread: &str) -> String {
    if let Some(n) = thread.strip_prefix("geode-query-") {
        return format!("query worker {n}");
    }
    if let Some(source) = thread.strip_prefix("geode-fetch-") {
        return format!("fetch {source}");
    }
    if let Some(source) = thread.strip_prefix("geode-subscribe-") {
        return format!("subscription {source}");
    }
    if let Some(target) = thread.strip_prefix("geode-egress-") {
        return format!("egress {target}");
    }
    match thread {
        REQUEST_LOOP => "data service",
        "geode-ingest" => "ingest",
        "geode-discovery" => "discovery",
        "geode-pricing" => "pricing",
        other => other,
    }
    .to_string()
}

/// One segment for every stopped thread. The request loop outranks the
/// rest: once it is gone, nothing else the bar says describes a live
/// service. Two or more other threads collapse to a count.
fn stopped_segment(stopped: &[StoppedThread]) -> Option<StoppedSegment> {
    let first = stopped.first()?;
    if let Some(service) = stopped.iter().find(|t| t.thread == REQUEST_LOOP) {
        let others: Vec<&str> = stopped
            .iter()
            .filter(|t| t.thread != REQUEST_LOOP)
            .map(|t| t.label.as_str())
            .collect();
        let mut detail = service.reason.clone();
        if !others.is_empty() {
            detail.push_str(&format!("; also stopped: {}", others.join(", ")));
        }
        return Some(StoppedSegment {
            text: SharedString::new_static("data service stopped — restart Geode"),
            detail: detail.into(),
        });
    }
    if stopped.len() == 1 {
        return Some(StoppedSegment {
            text: format!("{} stopped", first.label).into(),
            detail: first.reason.clone().into(),
        });
    }
    let detail = stopped
        .iter()
        .map(|t| format!("{}: {}", t.label, t.reason))
        .collect::<Vec<_>>()
        .join("; ");
    Some(StoppedSegment {
        text: format!("{} data threads stopped", stopped.len()).into(),
        detail: detail.into(),
    })
}
```

`Diagnostics` fields (after `dropped_events`):

```rust
    /// Submissions the data handle refused because its queue was full, since
    /// launch (the handle's own counter, read by the bridge each drain).
    pub refused: u64,
    /// Data threads that died, in the order they were reported.
    pub stopped: Vec<StoppedThread>,
    /// The status segment for `stopped`, rebuilt when a thread stops.
    stopped_segment: Option<StoppedSegment>,
```

initialised `refused: 0, stopped: Vec::new(), stopped_segment: None,`; methods:

```rust
    /// Record a data thread's death. A thread already recorded is a no-op:
    /// each dies once, and a redelivered event must not duplicate it.
    pub fn note_thread_stopped(&mut self, thread: &str, reason: String, at: SystemTime) {
        if self.stopped.iter().any(|t| t.thread == thread) {
            return;
        }
        self.stopped.push(StoppedThread {
            thread: thread.to_string(),
            label: thread_label(thread),
            reason,
            at,
        });
        self.stopped_segment = stopped_segment(&self.stopped);
        self.version += 1;
        // `sections::sources_rows` renders the stopped threads.
        self.versions.sources += 1;
    }

    /// The data handle's running total of `Busy` refusals. The same total
    /// again does not bump.
    pub fn note_refused(&mut self, total: u64) {
        if self.refused == total {
            return;
        }
        self.refused = total;
        self.version += 1;
    }

    /// The prepared stopped segment, `None` while every data thread lives.
    pub fn stopped_segment(&self) -> Option<&StoppedSegment> {
        self.stopped_segment.as_ref()
    }
```

`build_summary`, after the dropped segment:

```rust
        if self.refused > 0 {
            parts.push(format!("{} refused", self.refused));
        }
```

(update the `summary` doc to name the refused segment).

`geode-shell/src/shell/status.rs`: add the parameter `stopped: Option<&crate::diagnostics::StoppedSegment>,` immediately before `diagnostics_summary`, change `on_diagnostics_click: impl Fn(&mut Window, &mut App) + 'static` to `+ Clone + 'static`, and paint it first (before the count prefix):

```rust
    let mut bar = StatusBar::new().flex_none().w_full().h_full();
    if let Some(segment) = stopped {
        // First and in danger: a stopped data thread outranks every count
        // after it, which may describe a service that no longer runs. It
        // never clears; restarting Geode is the recovery.
        let stopped_click = on_diagnostics_click.clone();
        bar = bar.left(
            div()
                .id("data-stopped")
                .px_1()
                .rounded(theme.radius_tokens().sm)
                .text_color(theme.danger)
                .pointer_states(control::paint(
                    theme,
                    control::Rest::Bare,
                    theme.status_bar,
                    theme.danger,
                ))
                .debug_selector(|| "data-stopped".to_string())
                .child(segment.text.clone())
                .tooltip(crate::tips::tip_with(
                    SharedString::new_static("tip-data-stopped"),
                    segment.detail.clone(),
                    None,
                    Some(SharedString::new_static("click to open diagnostics")),
                ))
                .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                    stopped_click(window, cx);
                }),
        );
    }
```

Update the function's doc comment's segment order ("… stopped data threads first, then count, …; the stopped segment uses danger and opens the diagnostics tile").

`geode-shell/src/shell/render.rs`: after `let diagnostics_summary = diagnostics_read.summary();` add `let stopped = diagnostics_read.stopped_segment();` and pass `stopped,` before the summary argument. Update every other `status_bar(` call (tests in `shell/status.rs` or `shell/tests/*` — find with `grep -rn "status::status_bar(\|status_bar(" crates/geode-shell/src`) with `None,` in that position.

`geode-diagnostics/src/sections.rs`, at the top of `sources_rows` after `let mut out = Vec::new();`:

```rust
    // A stopped data thread leads the section the status segment opens:
    // it is why the bar went red, and it outlives every source row below.
    if !d.stopped.is_empty() {
        out.push(row("stopped threads — restart Geode to recover them", 0, Tone::Error));
        for t in &d.stopped {
            out.push(row(
                format!("{}: {} (at {})", t.label, t.reason, local_hms(t.at, clock)),
                1,
                Tone::Error,
            ));
        }
    }
```

(Move `let mut out = Vec::new();` above the reported/unreported sort if it is below it.)

`geode-app/src/bridge.rs` drain: before the `window.update(..)` call add `let now_refused = refused_handle.dropped_requests();` (with `let refused_handle = handle.clone();` captured into the spawned task beside `diagnostics_for_drain`), inside the update after the dropped block:

```rust
                if now_refused != last_refused {
                    diagnostics.update(cx, |d, cx| {
                        let before = d.version();
                        d.note_refused(now_refused);
                        if d.version() != before {
                            cx.notify();
                        }
                    });
                }
```

with `let mut last_refused = 0u64;` beside `last_dropped` and `last_refused = now_refused;` beside `last_dropped = now_dropped;`. Replace Task 1's temporary arm:

```rust
                    // A data thread died despite containment. Its segment and
                    // the diagnostics row stay until restart; the data layer
                    // already logged it and the crash hook wrote its file.
                    DataEvent::ThreadStopped { thread, reason } => {
                        diagnostics.update(cx, |d, cx| {
                            let before = d.version();
                            d.note_thread_stopped(&thread, reason, SystemTime::now());
                            if d.version() != before {
                                cx.notify();
                            }
                        });
                    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run the Step 2 commands. Expected: PASS.

- [ ] **Step 5: Gate and commit**

Run: `cargo test -p geode-shell`, `cargo test -p geode-diagnostics`, `cargo test -p geode-app`, `cargo clippy -p geode-shell -p geode-diagnostics -p geode-app --all-targets -- -D warnings`, `cargo check -p geode-shell --features test-support --all-targets`, `cargo fmt --check`, `zsh scripts/mutation-check.sh --anchors-only`.

```bash
git add crates/geode-shell crates/geode-diagnostics crates/geode-app
git commit -m "feat(shell): stopped data threads and refused submissions in the status bar

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

- [ ] **Step 6: Mutation entries**

Existing entries on these files are untouched (checked: status.rs anchors are the pending-keys, fullscreen, height and ingest-strip lines; diagnostics.rs anchors include `        if self.dropped_events > 0 {`, unchanged). New:

```
run_mutation "diagnostics: a stopped thread gets no segment" \
  crates/geode-shell/src/diagnostics.rs \
  '        self.stopped_segment = stopped_segment(&self.stopped);' \
  '        self.stopped_segment = None;' \
  geode-shell one_stopped_thread_is_named_with_its_reason_in_the_tooltip

run_mutation "diagnostics: the request loop does not outrank" \
  crates/geode-shell/src/diagnostics.rs \
  '    if let Some(service) = stopped.iter().find(|t| t.thread == REQUEST_LOOP) {' \
  '    if let Some(service) = stopped.iter().find(|_| false) {' \
  geode-shell a_stopped_request_loop_outranks_the_other_threads

run_mutation "diagnostics: several stopped threads are not collapsed" \
  crates/geode-shell/src/diagnostics.rs \
  '    if stopped.len() == 1 {' \
  '    if !stopped.is_empty() {' \
  geode-shell two_stopped_threads_collapse_to_a_count_listing_each

run_mutation "diagnostics: a thread reported twice is recorded twice" \
  crates/geode-shell/src/diagnostics.rs \
  '        if self.stopped.iter().any(|t| t.thread == thread) {' \
  '        if false {' \
  geode-shell a_thread_reported_twice_is_recorded_once

run_mutation "diagnostics: refused submissions are not summarised" \
  crates/geode-shell/src/diagnostics.rs \
  '        if self.refused > 0 {' \
  '        if false {' \
  geode-shell refused_submissions_show_in_the_summary_and_are_omitted_at_zero

run_mutation "status: the stopped segment is not painted" \
  crates/geode-shell/src/shell/status.rs \
  '    if let Some(segment) = stopped {' \
  '    if let Some(segment) = stopped.filter(|_| false) {' \
  geode-shell a_stopped_thread_paints_the_stopped_segment_first

run_mutation "status: the stopped segment's click does nothing" \
  crates/geode-shell/src/shell/status.rs \
  '                    stopped_click(window, cx);' \
  '                    let _ = (&stopped_click, window, cx);' \
  geode-shell clicking_the_stopped_segment_opens_the_diagnostics_tile

run_mutation "sections: stopped threads are not listed" \
  crates/geode-diagnostics/src/sections.rs \
  '    if !d.stopped.is_empty() {' \
  '    if false {' \
  geode-diagnostics stopped_threads_lead_the_sources_section

run_mutation "bridge: a stopped thread never reaches diagnostics" \
  crates/geode-app/src/bridge.rs \
  '                            d.note_thread_stopped(&thread, reason, SystemTime::now());' \
  '                            let _ = (&thread, reason);' \
  geode-app a_thread_stopped_event_reaches_the_status_segment

run_mutation "bridge: refused submissions are not read" \
  crates/geode-app/src/bridge.rs \
  '                        d.note_refused(now_refused);' \
  '                        let _ = now_refused;' \
  geode-app refused_submissions_reach_the_status_summary
```

Verify each; commit `test(mutation): stopped and refused status entries`.

---

### Task 7: Busy versus stopped at every call site

**Files:**
- Modify: `crates/geode-blotter/src/tile.rs`, `crates/geode-marketdata/src/tile.rs`, `crates/geode-timeseries/src/tile/data.rs`, `crates/geode-timeseries/src/tile/tests.rs`, `crates/geode-pricer/src/tile.rs`, `crates/geode-pricer/src/store.rs`, `crates/geode-app/src/bridge.rs`, `scripts/mutation-check.sh`

**Interfaces:**
- Consumes: `Refusal` + `Display`, `fill_for_tests`, `SheetStore` Results, `Loaded::Refused(Refusal)` (Task 1).
- Produces (pricer): `REFUSED = "pricing request refused: the data service is busy; retrying"`, `STOPPED = "pricing request refused: the data service has stopped"`, `NOT_SAVED = "sheet not saved: the data service is busy; the next edit retries"`, `SAVE_STOPPED = "sheet not saved: the data service has stopped"`, `fn load_refused(refusal: Refusal) -> String`, tile fields `stopped: bool`, `save_stopped: bool`; `MemorySheetStore::{set_save_refusal(Option<Refusal>), set_load_refusal(Option<Refusal>), set_forget_refusal(Option<Refusal>)}` (the existing `set_refusing(bool)`/`set_load_refused(bool)` stay as `Busy` shorthands).

- [ ] **Step 1: Write the failing tests**

Blotter (`crates/geode-blotter/src/tile.rs` tests):

```rust
    #[gpui::test]
    fn a_refused_query_says_busy_or_stopped(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.tile.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        let first = next_query(&h.requests);
        deliver(&h, &mut vcx, first.tag, Ok(snapshot()));
        let change = |vcx: &mut gpui::VisualTestContext| {
            h.frame.update(vcx, |f, cx| {
                f.set_as_of(AsOf::At(chrono::Utc::now()));
                cx.notify();
            });
        };
        let error = |vcx: &gpui::VisualTestContext| {
            h.tile.read_with(vcx, |t, _| t.error.as_ref().map(|e| e.0.clone()))
        };
        h.data.fill_for_tests();
        change(&mut vcx);
        assert_eq!(error(&vcx).as_deref(), Some("query refused: the data service is busy"));
        h.data.shutdown();
        change(&mut vcx);
        assert_eq!(error(&vcx).as_deref(), Some("query refused: the data service has stopped"));
    }
```

Market data: in `a_refused_request_arrives_at_the_barrier_and_retries` change the expected notice to `"document request refused: the data service has stopped"`; in `a_refused_submit_says_so_and_sends_nothing` to `"upload refused: the data service has stopped"`. Add busy variants:

```rust
    #[gpui::test]
    fn a_busy_document_refusal_says_busy(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.data.fill_for_tests();
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some("document request refused: the data service is busy".to_string())
        );
    }

    #[gpui::test]
    fn a_busy_upload_refusal_says_busy_and_clears_in_flight(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_upload(cx);
        h.with_document(&mut vcx);
        h.edit_one_cell(&mut vcx);
        h.command(&mut vcx, "upload").unwrap();
        draw(&mut vcx);
        h.data.fill_for_tests();
        type_keys(&mut vcx, "y");
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some("upload refused: the data service is busy".into())
        );
        assert!(h.tile.read_with(&vcx, |t, _| t.sent.is_none() && t.in_flight.is_none()));
    }
```

(If `open_upload`'s harness drains requests between steps so the queue is not full at `y`, call `fill_for_tests` immediately before `type_keys`, as written.)

Timeseries: add `data: DataHandle` to `Harness` (store `data.clone()` where the harness is built at `let (data, rx) = DataHandle::for_tests();`) and:

```rust
#[gpui::test]
fn a_refused_fetch_fails_the_chip_by_kind(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.visible(&mut vcx, true);
    h.data.fill_for_tests();
    h.command(&mut vcx, "add SPX.close").unwrap();
    assert!(matches!(&h.model(&vcx).slots()[0].state,
        SlotState::Failed(e) if e == "fetch refused: the data service is busy"));
    h.close_channel();
    h.command(&mut vcx, "add NDX.close").unwrap();
    assert!(matches!(&h.model(&vcx).slots()[1].state,
        SlotState::Failed(e) if e == "fetch refused: the data service has stopped"));
}
```

In `a_refused_submit_notices_and_still_answers_the_barrier` tighten the notice assertion to `== Some("series request refused: the data service has stopped")` (it uses `close_channel`).

Pricer (`crates/geode-pricer/src/tile.rs` tests): in `a_refused_submission_notices_and_retries_after_a_second`, `a_refused_saves_notice_outlives_pricing_notices_and_escape`, `a_refusal_with_nothing_left_to_price_clears`, `consecutive_refusals_back_off`, `a_load_starting_clears_a_refusal_streak`, replace `h.close_channel();` with `h.fill_queue();` (they test the busy streak). Replace `blocked_notice("book", LOAD_REFUSED)` with `blocked_notice("book", &load_refused(Refusal::Busy))`. Add:

```rust
    #[gpui::test]
    fn a_stopped_service_stops_the_pricing_backoff(cx: &mut gpui::TestAppContext) {
        let (store, record) = seeded(&BOOK);
        let (h, mut vcx) = open_full(cx, Some(record), store, PricerSettings::default());
        h.close_channel();
        h.visible(&mut vcx, true);
        assert_eq!(h.notice(&vcx).as_deref(), Some(STOPPED));
        assert!(h.tile.read_with(&vcx, |t, _| t.retry_task.is_none()), "no backoff is armed");
        let tag = h.tile.read_with(&vcx, |t, _| t.tag);
        settle(&mut vcx, RETRY_CAP);
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.tag), tag, "nothing retries");
        assert_eq!(h.notice(&vcx).as_deref(), Some(STOPPED), "and it keeps saying why");
    }

    #[gpui::test]
    fn a_stopped_service_stops_save_retries(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let base = h.store.save_count();
        h.store.set_save_refusal(Some(Refusal::Stopped));
        edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 2 });
        settle(&mut vcx, SAVE_IDLE);
        assert_eq!(h.save_notice(&vcx).as_deref(), Some(SAVE_STOPPED));
        h.store.set_save_refusal(None);
        edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 3 });
        settle(&mut vcx, SAVE_IDLE);
        assert_eq!(h.store.save_count(), base, "no later edit asks a stopped store again");
        assert_eq!(h.save_notice(&vcx).as_deref(), Some(SAVE_STOPPED));
    }

    #[gpui::test]
    fn a_stopped_load_names_the_stopped_service(cx: &mut gpui::TestAppContext) {
        let (store, record) = seeded(&BOOK);
        store.set_load_refusal(Some(Refusal::Stopped));
        let (h, mut vcx) = open_full(cx, Some(record), store, PricerSettings::default());
        h.visible(&mut vcx, true);
        assert_eq!(
            h.save_notice(&vcx),
            Some(blocked_notice("book", &load_refused(Refusal::Stopped)).to_string())
        );
    }

    #[gpui::test]
    fn a_stopped_remove_names_the_stopped_service(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.store.save("gone", sheet_rows("gone", &["NKY Z26 30000 C"])).unwrap();
        h.store.set_forget_refusal(Some(Refusal::Stopped));
        h.command(&mut vcx, "rm gone").unwrap();
        type_keys(&mut vcx, "y");
        assert_eq!(
            h.footer(&vcx).as_deref(),
            Some("sheet 'gone' not removed: the data service has stopped")
        );
    }
```

(Use the harness's own helpers for the `:rm` confirm and the footer read; `grep -n "fn .*rm\|fn footer" crates/geode-pricer/src/tile.rs` finds the existing `:rm` tests to copy the two calls from.)

Bridge (`crates/geode-app/src/bridge.rs` tests): in `a_refused_distinct_request_errors_the_picker` (the handle is shut down) change the expected value to `"the data service has stopped".to_string()`; add a busy twin that calls `handle.fill_for_tests()` instead of `handle.shutdown()` and expects `"the data service is busy — try again"`. Add:

```rust
    #[gpui::test]
    fn a_stopped_service_does_not_retry_the_catalog(cx: &mut gpui::TestAppContext) {
        let f = catalog_fixture(cx);
        let mut vcx = gpui::VisualTestContext::from_window(f.window.into(), cx);
        let shell = f.window.root(&mut vcx).unwrap().read_with(&vcx, |r, _| {
            r.view().clone().downcast::<ShellView>().unwrap()
        });
        let diagnostics = shell.read_with(&vcx, |s, _| s.diagnostics().clone());
        f.bridge.handle.shutdown();
        diagnostics.update(&mut vcx, |d, cx| {
            d.watch();
            cx.notify();
        });
        vcx.run_until_parked();
        vcx.executor().advance_clock(CATALOG_RETRY_DELAY * 3);
        vcx.run_until_parked();
        assert!(
            !diagnostics.read_with(&vcx, |d, _| d.pending_catalog_request()),
            "a stopped service keeps no retry demand; the stopped segment says why"
        );
    }

    #[gpui::test]
    fn a_reload_into_a_stopped_service_is_an_error_diagnostic(cx: &mut gpui::TestAppContext) {
        let services = test_shell_services_with_sources(ConfigSources {
            builtin: vec![LayerDoc::builtin("views", "[tree]\ndataset = \"risk\"\n").unwrap()],
            desk: None,
            user: None,
        });
        let window = open_test_window(cx, services);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let (handle, _rx) = DataHandle::for_tests();
        handle.shutdown();
        let bridge = test_bridge(handle);
        cx.update(|cx| attach(&bridge, window, cx));
        let shell = window.root(&mut vcx).unwrap().read_with(&vcx, |root, _| {
            root.view().clone().downcast::<ShellView>().unwrap()
        });
        vcx.update(|_, cx| {
            shell.update(cx, |_, cx| cx.emit(ShellEvent::ConfigReloaded));
        });
        vcx.run_until_parked();
        let diagnostics = shell.read_with(&vcx, |s, _| s.diagnostics().clone());
        assert!(diagnostics.read_with(&vcx, |d, _| d.data_diagnostics.iter().any(|(_, d)| {
            d.severity == Severity::Error
                && d.message == "the reloaded views did not reach the data service: the data service has stopped"
        })));
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-blotter --lib a_refused_query_says`, `cargo test -p geode-marketdata --lib refus`, `cargo test -p geode-timeseries --lib refused`, `cargo test -p geode-pricer --lib stopped`, `cargo test -p geode-app --lib stopped_service`, `cargo test -p geode-app --lib a_reload_into_a_stopped`, `cargo test -p geode-app --lib refused_distinct`
Expected: FAIL (old wording, missing constants and knobs).

- [ ] **Step 3: Implement**

Blotter:

```rust
        if let Err(refusal) = queued {
            self.error = Some((format!("query refused: {refusal}"), Tone::DangerText));
```

(the rest of the block unchanged: a stopped refusal retries on the next frame change too, since each attempt costs nothing and re-reports).

Market data document site:

```rust
        self.query_in_flight = queued.is_ok();
        if let Err(refusal) = queued {
            self.notice = Some(format!("document request refused: {refusal}").into());
```

Upload site:

```rust
        if let Err(refusal) = queued {
            self.notice = Some(format!("upload refused: {refusal}").into());
```

Timeseries fetch:

```rust
            match queued {
                Ok(()) => {
                    self.in_flight.insert((source, identity));
                }
                // Nothing is coming, and a chip left `Fetching` for ever
                // would say the opposite.
                Err(refusal) => self.model.set_pair_state(
                    &source,
                    &identity,
                    SlotState::Failed(format!("fetch refused: {refusal}")),
                ),
            }
```

Timeseries series:

```rust
            Some(params) => {
                let queued = self.data.series(params);
                if let Err(refusal) = queued {
                    self.notice = Some(format!("series request refused: {refusal}").into());
                }
                queued.is_ok()
            }
```

Pricer constants (replace `REFUSED`, `NOT_SAVED`, `LOAD_REFUSED`):

```rust
pub(crate) const REFUSED: &str = "pricing request refused: the data service is busy; retrying";
/// The data service has stopped: no retry can succeed, so none is armed.
pub(crate) const STOPPED: &str = "pricing request refused: the data service has stopped";
pub(crate) const NOT_SAVED: &str = "sheet not saved: the data service is busy; the next edit retries";
/// The data service has stopped: later edits are not saved either.
pub(crate) const SAVE_STOPPED: &str = "sheet not saved: the data service has stopped";

/// Why a load the store never submitted failed (`Loaded::Refused`).
pub(crate) fn load_refused(refusal: Refusal) -> String {
    format!("the store refused the load: {refusal}")
}
```

Fields (beside `refusals` and `save_refused`):

```rust
    /// A pricing submission was refused `Stopped`. The service never comes
    /// back, so no backoff is armed and the header says stopped from here on.
    stopped: bool,
    /// A save was refused `Stopped`: later edits do not ask the store again.
    save_stopped: bool,
```

`submit` refusal branch:

```rust
        if queued.is_ok() {
            self.in_flight = flight;
            self.end_refusals();
        } else if queued == Err(Refusal::Stopped) {
            // A stopped service does not come back: arming the backoff would
            // retry for the life of the tile against a refusal that repeats.
            self.in_flight.clear();
            self.end_refusals();
            self.stopped = true;
        } else {
            (existing busy body unchanged)
        }
```

`rebuild_chrome`:

```rust
        let notice = if self.stopped {
            Some(STOPPED.into())
        } else if self.refusals > 0 {
            Some(REFUSED.into())
        } else {
            self.notice.clone().or_else(|| self.view_notice.clone())
        };
```

`save_now`:

```rust
        if self.save_blocked {
            return true;
        }
        // A stopped store refuses every save; asking again on each edit only
        // repeats the refusal.
        if self.save_stopped {
            return false;
        }
        let Some(rows) = to_rows(&self.sheet) else {
            return true;
        };
        let saved = self.shared.store.save(&self.sheet.name, rows);
        if saved.is_ok() {
            (existing accepted body)
        } else {
            self.save_refused = true;
            self.save_notice = Some(NOT_SAVED.into());
            if saved == Err(Refusal::Stopped) {
                self.save_stopped = true;
                self.save_notice = Some(SAVE_STOPPED.into());
            }
            false
        }
```

Load sites: `Loaded::Refused(refusal) => { blocked = Some(blocked_notice(&name, &load_refused(refusal))); … }` and `Loaded::Refused(refusal) => Err(load_refused(refusal)),`.

Forget sites:

```rust
                    } else {
                        match self.shared.store.forget(&old) {
                            // Reserved until the forget is answered.
                            Ok(()) => self.forgetting.push(old),
                            Err(refusal) => {
                                self.shared.retiring.borrow_mut().remove(&old);
                                self.notice =
                                    Some(format!("old sheet '{old}' not removed: {refusal}").into());
                            }
                        }
                    }
```

and for `:rm`:

```rust
        } else {
            match self.shared.store.forget(&pending.sheet) {
                Ok(()) => {
                    // Reserved until the forget is answered.
                    self.shared.retiring.borrow_mut().insert(pending.sheet.clone());
                    self.forgetting.push(pending.sheet);
                }
                Err(refusal) => {
                    self.footer =
                        Some(format!("sheet '{}' not removed: {refusal}", pending.sheet).into());
                }
            }
        }
```

`MemorySheetStore`: replace `refusing: Rc<Cell<bool>>` and `load_refused: Rc<Cell<bool>>` with `save_refusal: Rc<Cell<Option<Refusal>>>`, `load_refusal: Rc<Cell<Option<Refusal>>>` and add `forget_refusal: Rc<Cell<Option<Refusal>>>`; `set_refusing(b)` → `self.save_refusal.set(b.then_some(Refusal::Busy))`; `set_load_refused(b)` → `self.load_refusal.set(b.then_some(Refusal::Busy))`; add `set_save_refusal`, `set_load_refusal`, `set_forget_refusal` setters; `load` answers `Loaded::Refused(r)` when `load_refusal` is `Some(r)`; `save` returns `Err(r)` when `save_refusal` is `Some(r)`; `forget` returns `Err(r)` (before any change) when `forget_refusal` is `Some(r)`.

Bridge distinct:

```rust
            ShellEvent::DistinctRequested(params) => {
                let queued = handle.distinct(params.clone());
                if let Err(refusal) = queued {
                    let outcome = DistinctOutcome {
                        key: params.key,
                        tag: params.tag,
                        column: params.column.clone(),
                        values: Err(match refusal {
                            Refusal::Busy => "the data service is busy — try again".into(),
                            Refusal::Stopped => "the data service has stopped".into(),
                        }),
                    };
                    shell.update(cx, |s, cx| s.deliver_distinct(outcome, cx));
                }
            }
```

Bridge catalog:

```rust
            match handle.catalog(CatalogParams {
                key: DIAGNOSTICS_KEY,
                tag,
                as_of,
            }) {
                Ok(()) => refresh.in_flight.set(Some((tag, request))),
                Err(Refusal::Busy) => {
                    tracing::warn!(
                        target: "geode::query",
                        "catalog request refused: the data service is busy; retrying"
                    );
                    refresh.retry(&diagnostics, request, window, cx);
                }
                // Nothing will serve a retry, and the stopped segment already
                // says why.
                Err(Refusal::Stopped) => {}
            }
```

Bridge `replace_views` (collect into `reload_diags`):

```rust
                let handoff = match handle.replace_views(views, dims) {
                    Ok(()) => None,
                    Err(refusal) => Some(Diagnostic {
                        severity: match refusal {
                            Refusal::Busy => Severity::Warning,
                            Refusal::Stopped => Severity::Error,
                        },
                        layer: None,
                        file: None,
                        message: format!(
                            "the reloaded views did not reach the data service: {refusal}"
                        ),
                        path: None,
                    }),
                };
```

and chain it into `reload_diags` on its own line:

```rust
                let reload_diags: Vec<Diagnostic> = presentation_diags
                    .into_iter()
                    .chain(colour_diags)
                    .chain(pin_diags)
                    .chain(handoff)
                    .collect();
```

Imports: `use geode_data::Refusal;`, `use geode_core::config::Severity;` as needed.

- [ ] **Step 4: Run tests to verify they pass**

Run the Step 2 commands; Expected: PASS.

- [ ] **Step 5: Gate and commit**

Run: `cargo test -p geode-blotter -p geode-marketdata -p geode-timeseries -p geode-pricer -p geode-app`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check`. (`--anchors-only` after Step 6.)

```bash
git add crates
git commit -m "feat(modules): refusals say busy or stopped, and stopped ends the retries

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

- [ ] **Step 6: Mutation entries**

Re-aim `bridge: a refused distinct request errors the picker instead of leaving it loading forever` (its `if !queued {` line is gone):

```
run_mutation "bridge: a refused distinct request errors the picker instead of leaving it loading forever" \
  crates/geode-app/src/bridge.rs \
  '                if let Err(refusal) = queued {' \
  '                if let (false, Err(refusal)) = (true, queued) {' \
  geode-app a_refused_distinct_request_errors_the_picker
```

Confirm the substring anchor `refresh.retry(&diagnostics, request, window, cx);` of `catalog refresh: refused submissions retry` still matches once, and that `pricer: a save refused at submission is counted as queued` (anchor `            self.save_refused = true;\n            self.save_notice = Some(NOT_SAVED.into());`), `pricer tile: a refusal destroys the standing notice` (`            self.refusals = self.refusals.saturating_add(1);`) and `mdtile: a refused submit arrives and clears acted` still match; each is inside a block kept at its indentation. New:

```
run_mutation "blotter: a stopped query refusal reads as something else" \
  crates/geode-blotter/src/tile.rs \
  '            self.error = Some((format!("query refused: {refusal}"), Tone::DangerText));' \
  '            self.error = Some(("query refused".to_string(), Tone::DangerText));' \
  geode-blotter a_refused_query_says_busy_or_stopped

run_mutation "mdtile: a document refusal loses its kind" \
  crates/geode-marketdata/src/tile.rs \
  '            self.notice = Some(format!("document request refused: {refusal}").into());' \
  '            self.notice = Some("document request refused".into());' \
  geode-marketdata a_busy_document_refusal_says_busy

run_mutation "mdtile: an upload refusal loses its kind" \
  crates/geode-marketdata/src/tile.rs \
  '            self.notice = Some(format!("upload refused: {refusal}").into());' \
  '            self.notice = Some("upload refused".into());' \
  geode-marketdata a_busy_upload_refusal_says_busy_and_clears_in_flight

run_mutation "timeseries: a refused fetch loses its kind" \
  crates/geode-timeseries/src/tile/data.rs \
  '                    SlotState::Failed(format!("fetch refused: {refusal}")),' \
  '                    SlotState::Failed("fetch refused".to_string()),' \
  geode-timeseries a_refused_fetch_fails_the_chip_by_kind

run_mutation "pricer tile: a stopped service still backs off" \
  crates/geode-pricer/src/tile.rs \
  '        } else if queued == Err(Refusal::Stopped) {' \
  '        } else if false {' \
  geode-pricer a_stopped_service_stops_the_pricing_backoff

run_mutation "pricer tile: a stopped overlay is not shown" \
  crates/geode-pricer/src/tile.rs \
  '        let notice = if self.stopped {' \
  '        let notice = if false {' \
  geode-pricer a_stopped_service_stops_the_pricing_backoff

run_mutation "pricer tile: a stopped store is asked again on the next edit" \
  crates/geode-pricer/src/tile.rs \
  '        if self.save_stopped {
            return false;
        }' \
  '' \
  geode-pricer a_stopped_service_stops_save_retries

run_mutation "pricer tile: a stopped save is not marked stopped" \
  crates/geode-pricer/src/tile.rs \
  '            if saved == Err(Refusal::Stopped) {' \
  '            if false {' \
  geode-pricer a_stopped_service_stops_save_retries

run_mutation "pricer tile: a refused load loses its kind" \
  crates/geode-pricer/src/tile.rs \
  '    format!("the store refused the load: {refusal}")' \
  '    { let _ = refusal; "the store refused the load".to_string() }' \
  geode-pricer a_stopped_load_names_the_stopped_service

run_mutation "pricer tile: a refused remove loses its kind" \
  crates/geode-pricer/src/tile.rs \
  '                        Some(format!("sheet '"'"'{}'"'"' not removed: {refusal}", pending.sheet).into());' \
  '                        Some(format!("sheet '"'"'{}'"'"' not removed", pending.sheet).into());' \
  geode-pricer a_stopped_remove_names_the_stopped_service

run_mutation "bridge: a stopped distinct reads as busy" \
  crates/geode-app/src/bridge.rs \
  '                            Refusal::Stopped => "the data service has stopped".into(),' \
  '                            Refusal::Stopped => "the data service is busy — try again".into(),' \
  geode-app a_refused_distinct_request_errors_the_picker

run_mutation "catalog refresh: a stopped service is retried" \
  crates/geode-app/src/bridge.rs \
  '                Err(Refusal::Stopped) => {}' \
  '                Err(Refusal::Stopped) => refresh.retry(&diagnostics, request, window, cx),' \
  geode-app a_stopped_service_does_not_retry_the_catalog

run_mutation "bridge: a view hand-off refusal is silent" \
  crates/geode-app/src/bridge.rs \
  '                    .chain(handoff)' \
  '                    .chain({ let _ = handoff; None::<Diagnostic> })' \
  geode-app a_reload_into_a_stopped_service_is_an_error_diagnostic

run_mutation "bridge: a stopped view hand-off is only a warning" \
  crates/geode-app/src/bridge.rs \
  '                            Refusal::Stopped => Severity::Error,' \
  '                            Refusal::Stopped => Severity::Warning,' \
  geode-app a_reload_into_a_stopped_service_is_an_error_diagnostic
```

Verify every entry (build-check, named caught, hand application), `--anchors-only`, commit `test(mutation): refusal call-site entries`.

---

### Task 8: Documentation

**Files:**
- Modify: `docs/current/data-path.md`, `docs/current/request-delivery.md`, `docs/current/shell.md`, `crates/geode-data/README.md`, `crates/geode-shell/README.md`, `crates/geode-pricer/README.md`

**Interfaces:** none (prose). Describe behaviour, failure semantics and limits; no task chronology.

- [ ] **Step 1: `docs/current/data-path.md`**

Add a section `## Containment and liveness` after `## Queues and shutdown` stating:
- every long-lived data thread (request loop `geode-data`, `geode-ingest`, `geode-discovery`, `geode-query-N`, `geode-pricing`, `geode-fetch-<source>`, `geode-subscribe-<source>`, `geode-egress-<target>`) is spawned through `supervise::spawn_supervised`; a body that unwinds past every boundary emits one `DataEvent::ThreadStopped { thread, reason }`, the crash file is still written, and nothing restarts the thread; a returning body declares nothing; a failed open emits `ThreadStopped` for `geode-data` beside its error diagnostic;
- the request loop contains each arm and the view-replacement step; a panicking request is answered exactly once, with an error naming the request kind and the payload, through the door that answers its success (table of arms → answers as in the spec §3, with Identities/Cancel/view replacement as one error diagnostic, and the view replacement keeping the previous views);
- `Refusal::{Busy, Stopped}`: `Busy` counts in `dropped_requests()`, `Stopped` does not; the stopped flag is set while a dying loop joins its workers;
- the five reported sites: identity listing (error diagnostic naming the source), pop-time stale check (fails open, warning diagnostic naming the file; the status summary counts only errors, so this shows in the diagnostics tile), local sweep (error diagnostic), discovery (health reason carries the payload), result delivery (built inside a boundary; a malformed distinct payload is that key's error);
- limits: no restart; the channel adapter's dispatcher and the demo bus are transport threads outside supervision; containment does not interrupt a blocked call.

In `## Egress and uploads`, replace the serialization text: encoding (`kind.write`) runs on the target's worker inside its own boundary; an encoding panic answers `egress '<target>': encoding panicked: <payload>`; exactly one `Upload` per accepted upload still holds; the queue bound is unchanged. Remove "serialization has no panic boundary".

- [ ] **Step 2: `docs/current/request-delivery.md`**

In `## Admission and completion`: submissions return `Result<(), Refusal>`; `Err(Busy)` means the queue was full, is counted, and a later submission can succeed; `Err(Stopped)` means the request loop has ended (panic, failed open, or shutdown), is not counted, and no retry can succeed; `cancel` still answers `bool`; `replace_views` refuses only `Stopped`. Add a sentence that an admitted request whose arm panics is answered with an error through its completion route. Update the `Document upload` row (serialization happens on the target worker) and the `## Document uploads` paragraph accordingly.

- [ ] **Step 3: `docs/current/shell.md`**

In the diagnostics/status section: the stopped segment (text forms: `<label> stopped`, `N data threads stopped`, `data service stopped — restart Geode`), tone danger, tooltip reasons, leads the left side, the request loop outranks other threads, never clears, click opens the diagnostics tile (Sources section, whose first rows list stopped threads with label, reason and time via the display clock); `N refused` in the summary beside `N dropped`, cumulative since launch, omitted at zero. Add `stopped`, `refused` to the `Diagnostics` state description and note `note_thread_stopped` bumps the `sources` counter.

- [ ] **Step 4: READMEs**

`crates/geode-data/README.md` `## Threading`: replace "Service-thread upload serialization has no equivalent boundary." with the supervision rule — "Every long-lived thread is spawned through `supervise::spawn_supervised`, which declares an unwinding body once as `DataEvent::ThreadStopped` and never restarts it. A new long-lived thread must use it, or its death is silent." — and the request-loop containment and `Refusal` sentences; update the `egress` table row ("per-target workers that encode and send").

`crates/geode-shell/README.md`: add `StoppedThread`/`StoppedSegment`/`thread_label` and the stopped segment to its diagnostics/status module map lines.

`crates/geode-pricer/README.md`: `Loaded::Refused(Refusal)`, `save`/`forget` return `Result<(), Refusal>`; a `Stopped` refusal ends the pricing backoff and save retries; the `MemorySheetStore` knobs.

- [ ] **Step 5: Verify and commit**

Run: `cargo fmt --check`, `zsh scripts/mutation-check.sh --anchors-only`, then the full CI set once: `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo check -p geode-shell --features test-support --all-targets`.

```bash
git add docs/current crates/geode-data/README.md crates/geode-shell/README.md crates/geode-pricer/README.md
git commit -m "docs: containment, thread supervision and refusals

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

**Display check owed (Matthew's screen):** the stopped segment's placement, wording and tone beside the summary; the `N refused` segment; the stopped-threads rows at the top of the Sources section.

---

## Self-review

- **Spec coverage.** §3 arms and the view step → Task 4 (probe for arms without a production route; catalog/fetch through an out-of-range coverage row). §4.1 helper and every listed thread → Tasks 1–2 (deviation 3 names the two unsupervised transport threads). §4.2 flag, `Busy`/`Stopped`, open failure, clean shutdown → Tasks 1 and 4. §5 five sites → Task 5; `kind.write` → Task 3. §6 segments, precedence, tile section, `stopped`/`refused` state, bridge fold and counter read → Task 6 (deviations 9, 10, 14). §7 every call-site row → Task 7 (deviations 7, 8). §8 failure semantics → Task 8 docs. §9 tests: helper (Task 1), serve per arm (Task 4), loop death (Task 4), open failure (Task 1), five sites (Task 5), egress (Task 3), shell pure and GPUI (Task 6), modules (Task 7), harness entries per task. §10 docs → Task 8.
- **Placeholders.** One conditional fallback remains and is explicit: the DuckDB literal for the out-of-range timestamp (Task 4). Anchors are written to rustfmt's expected shape; each task says to copy the formatted text if rustfmt wraps differently.
- **Type consistency.** `Refusal`, `fill_for_tests`, `REQUEST_LOOP`, `spawn_supervised(name: String, sink: EventSink, body)`, `unwatched()`, `tests_support::{recording, next_stop}`, `ServePoint`, `Probe`, `no_probe`, `spawn_with_probe`, `PanicAnswer`, `error_diagnostic`, `fail_fetch`, `FetchOutcome::IdentitiesPanicked`, `IngestEvent::Diagnostic`, `result_event`, `contained_result_event`, `StoppedThread`, `StoppedSegment`, `note_thread_stopped`, `note_refused`, `stopped_segment()`, `load_refused`, `STOPPED`, `SAVE_STOPPED` are each defined once and used with the same signatures in later tasks.
- **Review Focus.** Each of the five lines has its named test in its owning task.
