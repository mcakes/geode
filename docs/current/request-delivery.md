# Requests and UI delivery

This guide follows a request through `DataHandle`, the service, and the app
bridge. [The data path](data-path.md) covers ingestion and storage;
[the shell guide](shell.md) covers retained UI state and module hosting.

## Admission and completion

Ordinary `DataHandle` methods offer work to a 64-entry request channel under
a mutex. They do not wait for channel space, and return `Result<(), Refusal>`.
`Err(Busy)` means the queue was full: nothing was queued, the refusal counter
(`dropped_requests`) increments, and a later submission can succeed.
`Err(Stopped)` means the request loop has ended — it panicked, failed to open,
or was shut down: nothing was queued, the counter does not change, and no
retry can succeed. Neither creates an obligation to reply. `Ok` means
admission, not successful execution or storage publication. `cancel` still
answers `bool` (`true` means queued); its refusals follow the same counting
rule. The channel bound does not bound work already dispatched to other
workers.

The service opens its database and schema on its own thread. Opening failure
emits an error diagnostic and `ThreadStopped` for `geode-data`, marks the
handle stopped, and closes the receiver; requests admitted during startup do
not receive individual failure outcomes, and later submissions are refused
`Stopped`. After open, query compilation and validation failures return the
original request key and tag. An admitted request whose arm panics is answered
once with `<kind> request panicked: <payload>` through its completion route
below, and the loop serves on (see
[containment and liveness](data-path.md#the-request-loop)). Supersession and
cancellation can suppress query outcomes, and UI delivery may coalesce them.

| Request | Completion route |
|---|---|
| View or document query | `Query`, addressed by key and tag. |
| Series query | `Series`, addressed by key and tag, including cap/compile errors. |
| Distinct values | `Distinct`, with key, tag, and requested column. |
| Catalog | `Catalog`, read on the service thread and addressed by key/tag. |
| Reference table | `Reference`, read on the service thread and addressed by key/tag, with the dataset and as-of. |
| Snapshot poll-now | No dedicated reply. The source reports ordinary poll, health, and publication events; an unchanged snapshot has no publication. |
| Pricing | `Price`, addressed by key/tag; downstream queue refusal produces per-line errors. |
| Vol slices | `VolSlices`, addressed by key/tag, one result per job in job order; a full vol queue answers every job `the vol queue is full; resubmit`. |
| Local publish | Storage produces `Published` then `LocalPublished`. Any refusal or failure — the service refusing a dataset that is not local, the writer's validation or store error, a contained panic — produces an error diagnostic and `LocalPublishFailed`. Every admitted local publish answers exactly once. |
| Local forget | `Forgotten` (including a key that held nothing) or `ForgetFailed`. The service refuses a dataset that is not local or a key of the wrong arity with an error diagnostic and `ForgetFailed`; nothing is queued. |
| Document upload | `Upload`, addressed by tile key and upload tag; target validation and target-queue refusal (from the service thread), and encoding and transport results (from the target's worker) use the same outcome. |
| Move LHU | `Command`, addressed by command tag. Service/worker-queue refusal and the position adapter's answer use the same outcome. Acceptance by the position system does not update the grid; a later source snapshot carries the move. |
| History fetch | `SeriesFetched` identifies the source/identity pair, including zero-row completion. |
| Identity refresh | Updates a cache read by a later catalog request; worker refusal is logged, with no dedicated completion event. |

Tiles word a refusal by its kind, for example `query refused: the data
service is busy`, `upload refused: the data service has stopped`, or the vol
slice viewer's `document request refused: …` and `vol request refused: …`.
The blotter, market-data, timeseries and vol slice tiles do not retry on
their own: the next frame change or command submits again and repeats the
notice. The pricer
retries a busy pricing refusal with backoff and stops asking altogether after
a stopped one (see [the line pricer](features.md#pricing-and-the-line-pricer)).

Cancellation is itself an ordinary queued request and can be refused. It
targets query-pool, pricing and vol work by key (a running pricing or vol
batch stops at its next line or job boundary), does not cancel uploads,
fetch, position commands, or ingest work, and cannot retract a result already
emitted. It has no acknowledgement.
Receivers still need stale-result checks. See
[`handle.rs`](../../crates/geode-data/src/handle.rs).

## Document uploads

Upload admission has two stages. `DataHandle::upload` first offers the request
to the ordinary service queue. A refusal has no outcome. Once serviced,
`EgressWorkers` resolves the target and document kind, expands the address,
and offers the rows and kind, not yet encoded, to the target's eight-entry
queue. Each target has one worker that encodes and sends its jobs serially,
so encoding does not delay other service requests.

Validation and target-queue failures emit an error with the original tile key
and tag from the service thread. Encoding errors, transport results, and
contained encoding or transport panics use the same outcome path from the
worker; a panic produces a target-named error naming the step and leaves the
worker available for subsequent jobs. Because encoding follows queue
admission, a document that cannot be encoded occupies a queue slot until the
worker reaches it, and one sent to a full or unavailable target answers with
that refusal rather than its write error.

This is not a durable delivery receipt: startup failure, a blocked encoder or
transport, a worker that dies outside its boundaries (its queued jobs are
never answered), or event-sink refusal can prevent delivery. Egress has no automatic retry. An `Ok` acknowledges transport
success, not a new local generation; subscription ingestion and the panel's
echo check are separate. See [document egress](data-path.md#egress-and-uploads)
for configuration and worker details.

## View replacement and shutdown

`replace_views` stores the latest views and dimensions outside the request
queue, then offers a wakeup. A full queue still returns `Ok`: the service
checks the pending replacement before dispatching every dequeued request.
Multiple pending replacements collapse to the latest one. Its only refusal is
`Err(Stopped)` — the loop has ended, admission is closed, or the receiver is
disconnected — never temporary queue pressure; the bridge reports it as an
error diagnostic, because the service is then left on the old views while the
tile factory builds against the new ones. Acceptance does not acknowledge
validation or application.

`shutdown` closes admission for every handle clone, offers a best-effort
shutdown sentinel, drops the sender, and joins. Already queued requests run
before the sentinel; if the queue refused it, disconnection ends the loop
after dispatching those requests. Dropping the sender prevents an idle receive
from waiting forever, but does not interrupt service open or running I/O.

The service then stops its workers in dependency order, the ingest writer
last. The writer runs its queued local writes (the app's own publishes and
forgets) in order, each answering as usual, and drops every other queued job:
feed documents, series, reference snapshots and files are resent by their
sources after a restart. It is not a flush beyond that. Egress workers drain already queued uploads
before joining, with no transport timeout. The position worker also drains
accepted commands. Fetch calls, snapshot polls, discovery, publication,
uploads, and position commands can therefore delay joining. Final-handle drop
also joins on whichever thread releases it, so the app's quit hook runs
explicit shutdown on the background executor. See
[worker shutdown](data-path.md#queues-and-shutdown).

## The event mailbox

The app sink retains pending state under a mutex and signals a one-slot
wakeup channel. A full wakeup channel already promises a wakeup and does not
refuse the state. A closed receiver refuses; the bridge counts refusals and
logs closure once while producers continue. Acceptance means retained for
delivery, not applied to a window.

| Event | Pending-state rule |
|---|---|
| Query, series, distinct, catalog, price, vol slices | One entry per event kind and request key; a lower tag cannot replace a higher one. Equal tags replace. |
| Upload outcome | One entry per tile key and upload tag. Different uploads from one tile remain distinct; duplicate outcomes for the same pair replace. |
| Publication | One entry per dataset/batch; union affected books and retain the greatest generation ID. |
| Local-write outcome (saved, save failed, forgotten, forget failed) | Never coalesced: each is keyed by its arrival sequence and every one is delivered, in the writer's order. A writer may be waiting on one exact outcome (a pricer load deferred behind a queued save), so a later outcome for the same document must not replace it. The count is bounded by the writes the app queued, not by a feed's rate. |
| Position-command outcome | Never coalesced: each answer has its own arrival-sequence key. One command's success cannot hide another's refusal. |
| Reference-table outcome | Never coalesced. The bridge checks the request key and latest submitted tag before storing the answer in Diagnostics. |
| Fetch completion | Success clears an earlier failure for the pair. A later failure retains the earlier success as well, preserving its requery signal. |
| Loading / load ended | One shared progress entry; later state replaces earlier state. |
| Health / poll result | Latest entry per event kind and source. |
| Diagnostics | Merge distinct diagnostics and retain 256 in history order, trimming the oldest warnings and infos before any error (the shell ring's rule, `trim_keeping_errors`). |
| Thread stopped | One entry per thread name. Each thread stops once, so two different threads stopping before a drain are both delivered. |

Replacing a pending entry keeps its original position among pending keys.
This preserves state and invalidations, not every intermediate transition or
global event chronology. Memory follows pending keys and payload sizes, with
no fixed overall capacity. Displaced snapshots are released after unlocking
to reduce contention. The receiver consumes pending entries before awaiting
another wakeup. See [`events.rs`](../../crates/geode-app/src/events.rs).

## Routing into the window

The bridge awaits mailbox arrivals on the foreground executor and routes
every event through `window.update`. A closed window ends the drain on its
next event; while idle, the task can remain awaiting the mailbox. This is
arrival-driven delivery with no fixed frame-latency guarantee.

Keyed query, series, pricing, vol-slice, and upload results go to the matching
shell occupant;
absent occupants are ignored. Fetch completion broadcasts to visible
occupants, whose modules decide whether they watch that source/identity.
Distinct results go to the picker, which checks its current tag, column, and
open state. If submitting the picker's request fails, the bridge immediately
delivers a matching synthetic error rather than leaving it loading: `the data
service is busy — try again` or `the data service has stopped`.

`ThreadStopped` records the thread in `Diagnostics` for the status bar and the
diagnostics page. On every drained event the bridge also reads the handle's
`Busy` refusal total into `Diagnostics`, so the status summary's `N refused`
changes only when some event arrives: a refusal made while no events flow
appears at the next event. The catalog refresh retries a `Busy` refusal after
a delay and drops its demand on `Stopped`, since no retry can succeed and the
stopped segment already says why.

Every publication updates diagnostics. Non-local publications also advance
the frame's global data revision, matching dataset/document watches, and
recent-publication history. Local autosave skips those frame updates. The
history timestamp is event arrival time, not source freshness. Although the
mailbox retains the book union, the bridge currently records its count;
frame invalidation is by dataset or document batch, not individual book. A
document watch registered on a key prefix advances for a publish of any batch
under that prefix at a key-part boundary, so one watch on an underlying
follows every expiry of a two-part-key dataset such as `option_chain` (see
[workspace lanes](shell.md#workspace-lanes)).

Every local-write outcome for `pricer_sheets` goes to the pricer factory,
named by the sheet (the dataset's one-part key makes the batch the sheet
name): `LocalPublished` and `LocalPublishFailed` to `save_answered`,
`Forgotten` and `ForgetFailed` to `forget_answered`. The factory routes each
to the tile that queued it. No other dataset has a local writer, so other
datasets' outcomes are not routed. A forgotten document also requests a
watched catalog refresh, because a forget changes the catalog without a
publication. Failures also reach diagnostics as error diagnostics.

A document's publication entry and its local-write entries are separate
keys, and a replaced publication keeps its first position. So a `Published`
for a sheet can be delivered after that sheet's `Forgotten`. It is harmless:
for a local dataset a publication only notes the dataset in diagnostics (and
re-reads a watched catalog, which reads the database as it now is), and the
pricer reads only the local-write outcomes.

Catalog refresh permits one active request and coalesces follow-up demand.
Only the active key/tag releases that slot. Refusal or error retains demand
and retries after one second, with at most one retry timer. A successful
snapshot must match the current frame as-of; an old-era response schedules a
fresh request. Publication during a read allows its consistent snapshot to
display while retaining follow-up demand. Watched demand disappears when the
diagnostics page hides; explicit demand survives hiding. See
[diagnostics demand](shell.md#diagnostics-state-and-demand).

Health and progress update the diagnostics model. Service diagnostics append
in the retained data lane; they do not replace the shell's current config
diagnostics. Publication and health logging belong to the data service, so
the bridge avoids duplicate logs. See
[`bridge.rs`](../../crates/geode-app/src/bridge.rs).

Reference-table reads and poll-now requests have an independent lane from the
catalog. The bridge reads at the active workspace's as-of and stores only the
latest submitted reference tag; the page checks whether that answer matches
its selected dataset and as-of. Submission refusals are displayed separately
for reads and polls and are not automatically retried. Poll-now has no direct
answer, so its submission refusal clears on the next accepted submission.
Position-command outcomes go to `ShellView::note_command`, which replaces
the matching command's pending status notice; they are not tile deliveries.
