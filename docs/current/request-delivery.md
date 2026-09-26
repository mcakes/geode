# Requests and UI delivery

This guide follows a request through `DataHandle`, the service, and the app
bridge. [The data path](data-path.md) covers ingestion and storage;
[the shell guide](shell.md) covers retained UI state and module hosting.

## Admission and completion

Ordinary `DataHandle` methods offer work to a 64-entry request channel under
a mutex. They do not wait for channel space. `false` means no request was
queued, increments the refusal counter, and creates no obligation to reply.
`true` means admission, not successful execution or storage publication.
The channel bound does not bound work already dispatched to other workers.

The service opens its database and schema on its own thread. Opening failure
emits a diagnostic and closes the receiver; requests admitted during startup
do not receive individual failure outcomes. After open, query compilation and
validation failures return the original request key and tag. Supersession and
cancellation can suppress query outcomes, and UI delivery may coalesce them.

| Request | Completion route |
|---|---|
| View or document query | `Query`, addressed by key and tag. |
| Series query | `Series`, addressed by key and tag, including cap/compile errors. |
| Distinct values | `Distinct`, with key, tag, and requested column. |
| Catalog | `Catalog`, read on the service thread and addressed by key/tag. |
| Pricing | `Price`, addressed by key/tag; downstream queue refusal produces per-line errors. |
| Local publish | Storage produces `Published` then `LocalPublished`. Any refusal or failure — the service refusing a dataset that is not local, the writer's validation or store error, a contained panic — produces an error diagnostic and `LocalPublishFailed`. Every admitted local publish answers exactly once. |
| Local forget | `Forgotten` (including a key that held nothing) or `ForgetFailed`. The service refuses a dataset that is not local or a key of the wrong arity with an error diagnostic and `ForgetFailed`; nothing is queued. |
| History fetch | `SeriesFetched` identifies the source/identity pair, including zero-row completion. |
| Identity refresh | Updates a cache read by a later catalog request; worker refusal is logged, with no dedicated completion event. |

Cancellation is itself an ordinary queued request and can be refused. It
targets query-pool and pricing work by key, does not cancel fetch or ingest
work, and cannot retract a result already emitted. It has no acknowledgement.
Receivers still need stale-result checks. See
[`handle.rs`](../../crates/geode-data/src/handle.rs).

## View replacement and shutdown

`replace_views` stores the latest views and dimensions outside the request
queue, then offers a wakeup. A full queue still returns `true`: the service
checks the pending replacement before dispatching every dequeued request.
Multiple pending replacements collapse to the latest one. `false` means
admission is closed or the receiver is disconnected, not temporary queue
pressure. Acceptance does not acknowledge validation or application.

`shutdown` closes admission for every handle clone, offers a best-effort
shutdown sentinel, drops the sender, and joins. Already queued requests run
before the sentinel; if the queue refused it, disconnection ends the loop
after dispatching those requests. Dropping the sender prevents an idle receive
from waiting forever, but does not interrupt service open or running I/O.

The service then stops its workers in dependency order, the ingest writer
last. The writer runs its queued local writes (the app's own publishes and
forgets) in order, each answering as usual, and drops every other queued job:
feed documents, series and files are resent by their sources after a restart.
It is not a flush beyond that. Fetch calls, discovery, and publication can
delay joining. Final-handle drop also joins on whichever
thread releases it, so the app's quit hook runs explicit shutdown on the
background executor. See [worker shutdown](data-path.md#queues-and-shutdown).

## The event mailbox

The app sink retains pending state under a mutex and signals a one-slot
wakeup channel. A full wakeup channel already promises a wakeup and does not
refuse the state. A closed receiver refuses; the bridge counts refusals and
logs closure once while producers continue. Acceptance means retained for
delivery, not applied to a window.

| Event | Pending-state rule |
|---|---|
| Query, series, distinct, catalog, price | One entry per event kind and request key; a lower tag cannot replace a higher one. Equal tags replace. |
| Publication | One entry per dataset/batch; union affected books and retain the greatest generation ID. |
| Local-write outcome (saved, save failed, forgotten, forget failed) | Never coalesced: each is keyed by its arrival sequence and every one is delivered, in the writer's order. A writer may be waiting on one exact outcome (a pricer load deferred behind a queued save), so a later outcome for the same document must not replace it. The count is bounded by the writes the app queued, not by a feed's rate. |
| Fetch completion | Success clears an earlier failure for the pair. A later failure retains the earlier success as well, preserving its requery signal. |
| Loading / load ended | One shared progress entry; later state replaces earlier state. |
| Health / poll result | Latest entry per event kind and source. |
| Diagnostics | Merge distinct diagnostics and retain the latest 256 in history order. |

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

Keyed query, series, and pricing results go to the matching shell occupant;
absent occupants are ignored. Fetch completion broadcasts to visible
occupants, whose modules decide whether they watch that source/identity.
Distinct results go to the picker, which checks its current tag, column, and
open state. If submitting the picker's request fails, the bridge immediately
delivers a matching synthetic error rather than leaving it loading.

Every publication updates diagnostics. Non-local publications also advance
the frame's global data revision, matching dataset/document watches, and
recent-publication history. Local autosave skips those frame updates. The
history timestamp is event arrival time, not source freshness. Although the
mailbox retains the book union, the bridge currently records its count;
frame invalidation is by dataset or document batch, not individual book.

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
last diagnostics tile hides; explicit demand survives hiding. See
[diagnostics demand](shell.md#diagnostics-state-and-demand).

Health and progress update the diagnostics model. Service diagnostics append
in the retained data lane; they do not replace the shell's current config
diagnostics. Publication and health logging belong to the data service, so
the bridge avoids duplicate logs. See
[`bridge.rs`](../../crates/geode-app/src/bridge.rs).
