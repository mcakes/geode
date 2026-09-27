# Data-Layer Containment and Liveness — Design

Group D of the 2026-09-25 codebase review (`docs/superpowers/reviews/2026-09-25/`,
`SYNTHESIS.md` theme 2 and `data-service.md` C1, I4, M10). Approved in conversation
2026-09-27.

## 1. Problem

PHILOSOPHY §3 says silence is a bug. Three kinds of silence remain in the data
layer:

1. **The request loop is uncontained.** `serve` (`crates/geode-data/src/handle.rs`)
   is the only long-lived data thread without `catch_unwind`. Four arms run real
   work on it: `Catalog` SQL, `Upload`'s `kind.write` (foreign code), `Fetch`'s
   coverage read (an `.expect` on stored timestamps), and view compilation. A panic
   ends the thread. `rx` drops, every later submission takes the same `false` path
   as a full queue, and nothing in production reads the handle's `dropped` counter or
   the thread's `JoinHandle`. Every tile then says "busy or gone" forever, and some
   retry forever.
2. **An uncontained thread death is invisible.** The app's panic hook writes a
   crash file and returns, and no `Cargo.toml` sets `panic = "abort"`, so the process
   keeps running with a dead thread. Nothing restarts any thread, and nothing tells
   the trader.
3. **Five contained panics become no data.** These are reported nowhere a trader
   looks, or are reported without their cause:

   | Site | Today |
   |---|---|
   | Fetch worker, `Identities` work (`fetch.rs`, identity `None`) | log line only |
   | Ingest runner, pop-time stale check (`runner.rs`) | fails open (deliberately) but silently: nobody learns the catalog check failed |
   | Ingest runner, `sweep_local` (`runner.rs`) | `warn!` only |
   | Discovery poll (`scheduler.rs`) | Health `Failed{"discovery panicked"}`, payload discarded |
   | Query pool delivery (`query/pool.rs`) | the sink call, and an `.expect` building the `Distinct` payload (`service.rs`), run outside the worker's boundary, so a panic there kills the worker |

The egress worker's containment (review M1) was merged in group B and is not in
scope here.

## 2. Rulings (Matthew, 2026-09-27)

1. **Declare, never restart.** A data thread that dies despite containment is
   announced and stays dead until the app restarts. No automatic restart and no
   restart action: reopening DuckDB, re-subscribing and re-requesting from every
   tile is its own failure surface, and a panic that repeats on every request
   would crash-loop.
2. **Scope:** request-loop containment; the thread-exit signal; the five no-data
   panics; busy-versus-stopped refusals at every call site; moving `kind.write`
   off the request loop. **Out of scope:** making repeated identical health
   failures visible. The tracker emits only when a source's worst state changes,
   and that stays.
3. **Every signal surfaces in the status bar** (§6).

## 3. Per-request containment in `serve`

Each request arm runs inside `catch_unwind(AssertUnwindSafe(|| contained(..)))`,
the pattern every other data worker uses. A panicking request is answered through
the answer path it already has, with an error naming the request kind and the
panic payload (formatted by the existing shared payload-message helper, not a new
copy):

| Arm | Answer on panic |
|---|---|
| `Query`, `Document` | `DataEvent::Query { key, tag, snapshot: Err(..) }` |
| `Distinct` | `DataEvent::Distinct { key, tag, column, Err(..) }` |
| `Series` | `DataEvent::Series { key, tag, Err(..) }` |
| `Catalog` | `DataEvent::Catalog` carrying the error, in whatever form its payload already takes for a failed catalog read |
| `Price` | `DataEvent::Price` with every requested line `Err(..)` |
| `Upload` | `DataEvent::Upload { key, tag, target, result: Err(..) }` |
| `Fetch` | `DataEvent::SeriesFetched { source, identity, result: Err(..) }`, plus load-lane Health `Failed` for `identity@source`, matching the fetch worker's own panic outcome |
| `Publish` | `Diagnostics` plus `LocalPublishFailed`, as its refusal path does today |
| `Forget` | an Error `Diagnostic` plus `ForgetFailed` |
| `Identities`, `Cancel` | one Error `Diagnostic` naming the request kind (neither has an answer path) |

The pending-view-replacement step at the top of the loop is contained the same
way; a panic there is one Error `Diagnostic`, and the previous views stay in force.
After any contained panic the loop continues with the next request.

The shared rule: **a request that panics is answered exactly once, with an error, by
the same door that answers its success.** A tile never waits forever on a request
the loop swallowed.

## 4. The thread-exit signal

### 4.1 `spawn_supervised`

One helper in `geode-data` spawns every long-lived data thread:

```rust
fn spawn_supervised(name: String, sink: EventSink, body: impl FnOnce() + Send + 'static)
    -> std::io::Result<JoinHandle<()>>
```

It runs `body` inside a top-level `catch_unwind` **without** the `contained`
marker. So the crash hook still treats the panic as uncontained and writes its
crash file: an unexpected thread death is a genuine bug, and the file is the bug
report. If `body` unwinds, the helper emits exactly one
`DataEvent::ThreadStopped { thread: String, reason: String }` and returns. A body
that returns normally is a deliberate stop and emits nothing.

Threads spawned through it:
- the request loop (`geode-data`);
- the ingest runner (`geode-ingest`);
- discovery (`geode-discovery`);
- each query-pool worker (`geode-query-N`);
- each fetch, subscribe and egress worker;
- the pricing worker.

Any future long-lived data thread must use it. The crate README records that.

Thread names in the event are the spawn names above. The shell turns them into
readable labels (§6).

### 4.2 The handle knows before the UI does

The request loop's supervision also sets a shared `stopped` flag inside
`DataHandle`'s `Inner`. It is set on unwind, *before* `ThreadStopped` is emitted,
so a submission that races the event already sees it. `Inner::send` distinguishes:

- `TrySendError::Full` → `Refusal::Busy`, counted in `dropped` as today;
- `TrySendError::Disconnected`, or `stopped` set → `Refusal::Stopped`.

A deliberate `Shutdown` does not set `stopped`, so a quit is never reported as a
failure.

The review claimed that "`serve` exits by a route other than `Shutdown`" needs its
own signal. It does not: the only such routes are a panic, which `spawn_supervised`
reports, and an open failure, which already emits its `Diagnostic` and returns
normally before the loop starts. The open-failure case also sets `stopped`, since
the service never became available, and submissions then say `Stopped` rather
than `Busy`.

## 5. The five no-data panics, and `kind.write`

Every reason below carries the panic payload.

| Site | After |
|---|---|
| Fetch `Identities` | Error `Diagnostic`: `identity listing for <source> panicked: <payload>` |
| Pop-time stale check | **stays fail-open, and is reported** (Matthew, 2026-09-27). Fail-open is deliberate (`runner.rs`: a failed lookup must not silently discard the load), and the cost is a redundant reload that republishes the same data as a new generation, never a wrong total. The load still proceeds. A lookup panic or store error becomes an **Error** `Diagnostic` (a catalog row the lookup cannot read is corruption), naming the file and the payload or error, where today it is silent; it counts in `data N errors`. The existing `a_malformed_catalog_row_panics_the_pop_time_lookup_without_killing_the_runner` keeps its assertions and gains one for the diagnostic. |
| `sweep_local` | Error `Diagnostic`: `local sweep panicked: <payload>` |
| Discovery | the same Health `Failed`, reason `discovery panicked: <payload>` |
| Query-pool delivery | the event is *built* inside the worker's boundary, and the `.expect` in the `Distinct` payload becomes an `Err` answer to that key. Only the channel send stays outside, and the worker is supervised (§4.1), so if it dies anyway, that is declared. |

**`kind.write` moves onto the egress worker.** The `Upload` arm validates and
enqueues the rows. The target's worker runs `kind.write` and then `upload`, both
inside its existing boundary. A full queue still refuses immediately. An encoding
panic answers `Err("egress '<target>': encoding panicked: <payload>")` through the
worker's single answer door, so "exactly one `Upload` event per accepted upload"
still holds. The job type carries the rows and the document kind instead of the
encoded payload. The queue bound is unchanged.

## 6. Status bar

Every signal ends in a status-bar indicator. Each indicator opens the diagnostics
tile when clicked, like the existing summary.

| Signal | Indicator | Tone | Clears |
|---|---|---|---|
| Request loop stopped (panic or open failure) | **new**: `data service stopped — restart Geode`; reason in the tooltip | danger | never; restart only |
| Any other data thread stopped | **new**: `<label> stopped` (e.g. `ingest stopped`, `discovery stopped`, `query worker 2 stopped`), reason in the tooltip. Two or more collapse to `N data threads stopped`, with a tooltip listing each. | danger | never |
| Submissions refused because the loop was busy | **new**: `N refused`, backed by the handle's `dropped` counter | warning | never; cumulative since launch, like the existing `N dropped` |
| Contained panics answered as errors (§3, identities, sweep) | existing `data N errors`, fed by the new Error `Diagnostic`s | danger | as today |
| Discovery and stale-check panics | existing `sources … failed`, with the payload now in the detail | danger | the source's next clean result, as today |

**Placement and precedence.** The stopped segments lead the left side, before
every count. They outrank everything: once the request loop is dead, the source
and error counts describe a service that no longer exists. If the request loop and
other threads have all stopped, only the request-loop segment shows, and the
tooltip lists the rest.

**Diagnostics tile.** It gains a "stopped threads" section listing each thread's
label, reason and the time it stopped. Times come through `geode_core::clock::Clock`,
never `chrono::Local`.

**State.** `geode_shell::Diagnostics` gains `stopped: Vec<StoppedThread>` (label,
reason, time) and `refused: u64`. Both go into `build_summary` beside
`dropped_events`. The bridge folds `ThreadStopped` into `stopped` and reads
`DataHandle::dropped_requests()` each drain, as it already does for the sink's
dropped count. Segment strings are built when state changes, not per frame
(render discipline).

## 7. Refusals at the call sites

`DataHandle`'s submit methods return `Result<(), Refusal>`, where
`pub enum Refusal { Busy, Stopped }`, in place of `bool`.

| Call site | `Busy` | `Stopped` |
|---|---|---|
| Blotter query | error `query refused: the data service is busy`, retry on the next frame change (as today) | `query refused: the data service has stopped`, same retry trigger, since each attempt costs nothing and re-reports |
| Market-data document | notice as today, wording `busy` | notice `… has stopped` |
| Market-data upload | notice as today | notice `… has stopped`; in-flight state cleared as today |
| Timeseries fetch | chip `Failed("fetch refused: … busy")` | chip `Failed("fetch refused: … stopped")` |
| Timeseries series | notice as today | notice `… stopped` |
| Pricer price | warn once per streak, backoff retry (as today) | overlay says stopped; **the backoff loop stops retrying** |
| Pricer load / save / forget | as today | the same surfaces with `stopped` wording; save does not retry on the next edit while stopped |
| Picker distinct (`bridge.rs`) | `Err("the data service is busy — try again")` | `Err("the data service has stopped")` |
| Diagnostics catalog (`bridge.rs`) | warn and retry after 1 s (as today) | **no retry**; the stopped segment already says why |
| `replace_views` (`bridge.rs`) | result currently ignored → a `Busy` becomes a warning `Diagnostic` | an Error `Diagnostic` |

"Busy or gone" disappears from every message. Wording is final at plan time and
is subject to the display check.

## 8. Failure semantics summary

- A panicking request is answered once, with an error, by its own answer door;
  the loop keeps serving.
- A dying thread is declared once, with its payload. It stays declared, and the
  crash file is still written.
- A refused submission says whether to retry (`Busy`) or not (`Stopped`).
- No contained panic in `geode-data` ends as only a log line.

## 9. Testing

Test at the lowest layer that proves the behaviour, through production routes.

- **`spawn_supervised`:** a panicking body emits exactly one `ThreadStopped`
  carrying the payload; a returning body emits nothing; the crash-hook marker is
  *not* set during the body.
- **`serve`:** one test per answering arm (§3) drives a panic through a real
  route and asserts:
  - the key receives `Err` naming the panic;
  - the next request is still served;
  - no `ThreadStopped` is emitted.

  Routes include an upload through `PanickingKind`, a catalog over a malformed row,
  and a fetch over an out-of-range stored timestamp. Where no production route can
  panic an arm, the test injects the panic the same way the query pool's existing
  `a_panicking_query_degrades_its_view_without_wedging_it` does, and the plan names
  which arms need that.
- **Loop death:** a panic outside any arm (injected) emits `ThreadStopped{thread:
  "geode-data", ..}`, and later submissions return `Refusal::Stopped`. A clean
  `Shutdown` emits nothing and sets nothing.
- **Open failure** sets `stopped`; submissions return `Stopped`.
- **The five sites:** one test each, through existing fixtures (`PanickingFetch`,
  the malformed catalog row, the runner's publish-panic fixture).
- **Egress move:** an encoding panic in `kind.write` answers exactly one
  `Upload{Err("… encoding panicked …")}`, the worker survives, and a queued upload
  behind it still answers.
- **Shell, pure:** `Diagnostics` folds `ThreadStopped` into segments (single,
  collapsed, request-loop precedence); `refused` renders and is omitted at zero.
- **Shell, GPUI:** the stopped segment renders, and a click on it opens the
  diagnostics tile.
- **Modules:** the pricer backoff stops on `Stopped`; the diagnostics catalog does
  not retry on `Stopped`.
- **Harness:** every contract above gets a mutation entry naming its test,
  verified by a named run reporting `caught` and a hand application failing on an
  assertion. `--build-check` must pass for every new entry.

**Display check owed** on Matthew's screen: the stopped segment's placement,
wording and tone beside the existing summary; the `N refused` segment.

## 10. Documentation

In the same change:
- `docs/current/data-path.md`: failure semantics (§8), `ThreadStopped`,
  `Refusal`, and `kind.write` on the worker;
- `docs/current/shell.md`: the status-bar segments and precedence;
- `crates/geode-data/README.md`: the `spawn_supervised` rule for new threads;
- `docs/current/performance.md`: nothing, unless the egress move changes a
  recorded path.
