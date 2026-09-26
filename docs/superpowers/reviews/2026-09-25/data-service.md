# geode-data review (excluding `src/query/`)

Scope: `crates/geode-data` — service, handle, ingest (runner, scheduler, load, subscribe, fetch, coalesce, plan, split), source discovery, adapters, store (DuckDB, publish, document, series, catalog, retention, ddl), pricing worker, egress, health. Read-only; every finding cites code that was read. `src/query/` is another reviewer's; `pool.rs` was skimmed only for the lock scope around result delivery.

## (a) Summary

1. The layer is in good shape on its central contracts: one writer, bounded UI-facing submission with counted refusal, per-source two-lane health decided and emitted under one lock, transactional publish with reserved ids and a maintained generation summary. These are tested at the pure level and through the service.
2. The biggest gap is containment asymmetry: every worker wraps foreign code in `catch_unwind` + `contained`, but the **service request loop** (`serve`) and the **egress worker** do not. A panic on either silently kills a thread the app cannot detect (the process keeps running; the crash hook only logs/writes a file).
3. "Silence is a bug" has three live violations: a missing/unmounted directory reports `Ok` forever; dropped subscription messages are counted but never surfaced; `LoadOutcome`'s extra/missing-column notes never leave `load_file`.
4. Storage cost per publish grows with history: `refresh_enum` rescans live+archive on every document/file publish, staging tables are persistent (WAL-written) rather than TEMP, and document/measure archives have no production retention — so a subscribed source's per-message cost is unbounded over a session.
5. `DataService::open` (690 lines) is a god-function with ~10 copies of the same health-emit closure; `service.rs` still has a clear natural split (ingest side vs query side) that is also the seam a background daemon would need.

### Thread / channel topology (as found)

```
UI thread ─DataHandle::send (try_send, Mutex<Option<SyncSender>>)─▶ [sync_channel 64: Request] ─▶ thread "geode-data" (serve loop, owns DataService + one reader conn)
   │  replace_views: PendingViews Mutex<Option<..>> latest-wins + best-effort wakeup Request::ReplaceViews
   ▼
serve dispatch (handle.rs:296-397), NO catch_unwind:
   ├─ Query/Distinct/Document/Series ─▶ QueryPool::submit (Mutex<Queue>+Condvar, latest-wins per key)
   │        ─▶ threads "geode-query-N" (one reader conn each; catch_unwind around run only)
   │        ─▶ result_sink called UNDER the pool queue lock (pool.rs:351-378) ─▶ HealthTracker.load_lane (series) ─▶ EventSink
   ├─ Price ─▶ PricingWorker (Mutex<Queue>+Condvar, cap 64 keys, latest-wins/key) ─▶ "geode-pricing" ─▶ EventSink
   ├─ Upload ─▶ kind.write() on the service thread ─▶ [sync_channel 8: Job] ─▶ "geode-egress-<target>" (NO catch_unwind) ─▶ EventSink
   ├─ Fetch ─▶ coverage read on service reader ─▶ FetchWorker.request [sync_channel 64] ─▶ "geode-fetch-<source>" (catch_unwind)
   │        ─▶ FetchOutcomeSink ─▶ IngestHandle.submit_series | EventSink(SeriesFetched) | identities Mutex<BTreeMap>
   ├─ Publish (local) ─▶ IngestHandle.submit_document
   ├─ Catalog ─▶ build_catalog on service reader (synchronous) ─▶ EventSink
   └─ Identities ─▶ FetchWorker.request

"geode-discovery" (1 thread, own reader conn; polls sequentially; catch_unwind per poll)
        ─discover()+build_plan─▶ IngestHandle.submit(plan)  ─SchedulerSink─▶ HealthTracker.discovery lane ─▶ EventSink
"geode-subscribe-<source>" (per subscribed source; MessageSink = sync_channel 256 fed by adapter; Coalescer 1/key)
        ─submit_document─▶ IngestHandle            ─LoadReportSink─▶ HealthTracker.load lane ─▶ EventSink
"geode-channel-<bus>" (ChannelAdapter dispatcher; inbound sync_channel 256; snapshot registrations, push outside lock)

IngestHandle = Arc<(Mutex<Queue{documents: VecDeque, series: VecDeque, items: Vec}>, Condvar)>  — UNBOUNDED, no refusal
        ─▶ "geode-ingest" (owns Store writer; documents > series > files; catch_unwind per op; PlanComplete emitted UNDER the queue lock)
        ─IngestSink─▶ HealthTracker (Mutex<HashMap<source, Lanes>>; emit + tracing UNDER the lock) ─▶ EventSink

EventSink (app) = Mutex<pending map> + 1-slot wake channel (events.rs) ─▶ GPUI foreground bridge ─▶ window.update
Lock order observed: pool queue → tracker → mailbox; ingest queue → mailbox; tracker → mailbox/tracing. No cycle found.
Shutdown (service.rs:1697-1731): egress → fetchers → subscriptions → pool → pricing → scheduler → ingest; struct field order (499-537) mirrors it for Drop. Ingest does not drain its queues.
```

## (b) Findings

### Critical

**C1. The service request loop has no panic containment; a panic kills the data service silently.**
`crates/geode-data/src/handle.rs:296-397` (`serve`) dispatches every request with no `catch_unwind`. Work that runs on this thread includes foreign code and stored-data decoding: `EgressWorkers::upload` calls `kind.write(&p.rows)` (`egress.rs:213`), `DataService::fetch` reads coverage through `from_micros(..).expect("a stored timestamp is in range")` (`service.rs:1560-1568`, `store/series.rs:36-38`), `DataService::catalog` runs `build_catalog` SQL synchronously (`service.rs:1610-1641`), and `query`/`series` run `ViewSpec::validate`/`compile_series`. Every other thread in the crate wraps such calls (`runner.rs:368,457,586,635`; `subscribe.rs:290`; `fetch.rs:118`; `scheduler.rs:159`; `pricing/worker.rs:146,186`). If `serve` panics, the thread exits, `rx` drops, and every later `DataHandle` call returns `false` and bumps `dropped` (`handle.rs:83-97`) — no `DataEvent::Diagnostics` is emitted, and the app's panic hook writes a crash file but calls the previous hook and returns (`geode-app/src/crash.rs:59-135`), so the process keeps running with a dead service.
Impact: the app looks alive but every query/upload/fetch is refused forever; the only trace is a counter nobody reads (see M2).
Direction: wrap each `match req` arm in `catch_unwind(contained(..))`, answer the request's own key with an error outcome, and emit one `Diagnostics` error naming the request kind; additionally, have `serve` emit a terminal `Diagnostics` if it exits by any route other than `Shutdown`. Move `kind.write` onto the egress worker (it is foreign code and can be slow).

### Major

**M1. Egress worker is the one uncontained background boundary; a transport panic breaks the "exactly one `Upload`" contract and drops queued jobs.**
`egress.rs:151-165` (`work`) calls `egress.upload(..)` with no `catch_unwind`. `grep catch_unwind` finds none in `egress.rs`. On a panic the thread unwinds: the in-flight job's `answer` never runs (contract at `egress.rs:7-11` and README: "never both, never neither"), `rx` drops so up to `EGRESS_QUEUE_BOUND` queued jobs vanish unanswered, and later uploads answer `stopped` (`egress.rs:238-240`) with no diagnostic saying why. The uncontained panic also goes to the crash hook as a process crash artifact while the process keeps running.
Direction: same shape as `fetch.rs:118-166` — `catch_unwind(contained(|| egress.upload(..)))`, answer `Err("egress '<t>': transport panicked: ..")`, keep the loop alive. Add a `PanickingEgress` test fixture like `PanickingKind` (`ddl.rs` tests_support).

**M2. Dropped subscription messages are counted but never surfaced; the counter has no production reader.**
`adapter/mod.rs:70-79` (`MessageSink::push`) counts refusals; `subscribe.rs:150-160` exposes `SubscriptionWorker::refused()` whose doc says "the count is the only way to tell the two apart" — but the only callers are tests (`subscribe.rs:1083,1096`). The same is true of `ChannelFeed::refused` (`channel.rs:236`) and `DataHandle::dropped_requests` (`handle.rs:249`; only `handle.rs` tests). A receiver that falls behind therefore drops snapshots and the source reads `Ok`.
Direction: on each receive-loop iteration (`subscribe.rs:262-283`) compare the counter to the last seen value and report `Degraded { "N messages dropped since .." }` on the load lane under a fixed batch key (e.g. `"<source>:queue"`), clearing to `Ok` after a quiet interval; surface `dropped_requests` in the diagnostics tile.

**M3. A missing, unmounted or unreadable source directory reports `Ok` on every poll.**
`source/discovery.rs:47-56`: `glob::glob` errors and non-matching patterns `continue`; `fs::metadata` failures `continue`. `scheduler.rs:163-178`: `worst_health(&[])` is `None`, so the sink receives `Health::Ok` with an empty detail. The guide documents this ("an empty poll does not prove path accessibility") but under the philosophy a trader cannot tell "share is down" from "nothing new". A typo in `paths` or a share that never mounted is a permanently green source with zero rows.
Direction: distinguish "pattern matched nothing" from "pattern's parent directory does not exist / is not readable" (one `fs::metadata` on the glob's literal prefix) and report the latter as `Degraded`/`PendingTooLong` on the discovery lane; count glob/metadata errors and include them in `Polled`.

**M4. Payload schema drift is not detected at open; same-typed reorders misfile silently, count changes fail every load without naming the cause.**
`store/mod.rs:125-171` (`apply_schema`) is `CREATE TABLE IF NOT EXISTS`; `service.rs:544-548` runs it and nothing compares the existing table's columns to the generated DDL (`grep information_schema.columns|duckdb_columns` finds nothing outside a test). Publish moves rows positionally: `publish.rs:190-200,218-228` (`insert into {live} select *, gen, time from staging`), `document.rs:117-128` (staging columns from the *current* `document_columns()`), `split.rs:114-124`. A column added/removed in `datasets.toml` makes every load of that dataset `Failed` (surfaced per batch, but as a DuckDB column-count error, not "schema drift"); a same-typed reorder writes values into the wrong columns with `Ok` health. The catalog already has a migration precedent (`catalog.rs:79-81` `ALTER TABLE .. ADD COLUMN IF NOT EXISTS`).
Direction: at open, for each expected table read `duckdb_columns()` and diff (name, type, position) against the DDL; on mismatch emit a `Diagnostics` error naming table and column, and mark every source over that dataset `Failed { "schema drift: ..." }` on the discovery lane so loads are not attempted. This is a few dozen lines and turns the documented limitation into an explicit error.

**M5. `refresh_enum` rescans live + archive on every publish; per-message cost grows without bound for subscribed sources.**
`ddl.rs:159-193`: `drop type ..; create type .. as enum (select distinct col from live union select distinct col from archive)`. Called per categorical column on every file publish (`load.rs:214-245`) and every document publish (`document.rs:200-203`). The document family's key dimension is categorical by default (`geode-core/src/schema/mod.rs:1015-1019`: utf8 dimension ⇒ categorical), so every CVI/dividend message pays a full scan of a document archive that grows by one generation per message and is never swept (`retention.rs:1-7` "the application does not schedule these sweeps"; `docs/current/performance.md:114`). A busy feed's publish latency therefore rises over the day, and with it the trader's staleness.
Direction: refresh only when a staged distinct value is absent from `enum_range(NULL::type)` (one small query over the staging table), or refresh from live only on publish and from the archive on a periodic maintenance pass; and give document archives a production sweep (see Idea I3).

**M6. Staging tables are persistent tables written through the WAL; every load writes its payload twice.**
`load.rs:113-120` `create or replace table staging_raw as select .. from read_csv(..)`; `split.rs:114-124` `create or replace table {grain staging}`; `document.rs:117-128` and `series.rs:118-121` likewise. None are `TEMP`. Each load therefore performs catalog DDL and writes the full payload into the database file before the publish transaction copies it again into live; readers can also see `staging_*`. For a one-million-row CSV this doubles write volume on the cold-start path the budget cares about.
Direction: `CREATE OR REPLACE TEMP TABLE` (session-local to the writer connection, not checkpointed). Verify with the existing `benches/ingest.rs`.

**M7. Ingest document/series queues are unbounded with no per-key coalescing, so the newest document waits behind every older generation of the same key.**
`runner.rs:196-215` (`submit_document`, `submit_series`) push to `VecDeque`s with "no capacity limit, refusal, or deduplication"; `runner.rs:130-137` `Queue`. The coalescer (`coalesce.rs`) only collapses documents not yet submitted, and its window (`source_config.rs:99-101`, default 500 ms) is shorter than a publish under M5's growth. A burst of N messages for one key becomes N serial publishes (each archiving the previous generation, refreshing enums, and inserting catalog rows). This is documented (`data-path.md` "Queues and shutdown") but contradicts the stated rule that bounded submission returns refusal rather than growing.
Direction: coalesce in the runner queue by `(source, dataset, key)`: a newer job replaces an unpublished older one when its `source_time >= older.source_time` (the older never went live, so no archive history is lost that as-of could have selected); add a depth threshold that reports `Degraded { "ingest backlog N" }` on the load lane rather than growing silently.

**M8. `Catalog::lookup_by_path` unwraps every column; a legacy/NULL catalog row turns every discovery poll into `Failed: discovery panicked`.**
`store/catalog.rs:247-268`: eleven `row.get(..).unwrap()` calls on stored data (e.g. a NULL `mtime`, `loaded_at`, or `row_count` from a catalog written by an older build). Callers: `discovery.rs:145-150` on every candidate of every poll (inside the scheduler's containment, so the whole poll reports `Failed { "discovery panicked" }` at `scheduler.rs:195-203` and no file from that source ever loads) and the runner's pop-time check (`runner.rs:586-600`, fail-open). Every other catalog reader maps errors (`live_health`, `book_freshness`). A test proves the runner survives (`runner.rs:1565`), but the discovery side has no test and the health message hides the real cause.
Direction: replace the unwraps with `?` mapped to `StoreError::Sql` naming the column; add a discovery test with a NULL `mtime` row asserting the source still loads other files and the detail names the row.

**M9. `LoadOutcome`'s extra-column and missing-optional notes never leave `load_file`.**
`load.rs:36-52` documents `extra_columns` as "recorded so they surface in diagnostics rather than vanishing silently", plus `missing_optional`, `conflicts` (non-carried) and `cross_file`. The runner's `Published` arm (`runner.rs:661-671`) reads only `gen_id`, `partitions`, `rows`, `health`; only carried-dimension conflicts reach health via `degradations` (`load.rs:262-276`). `grep` finds no consumer of `extra_columns`/`missing_optional` outside `load.rs`. A CSV that grew an undeclared column, or that dropped an optional one, is accepted as `Ok` with nothing said. (`cross_file` is at least persisted to `attribute_conflicts`.)
Direction: carry a `notes: Vec<String>` on `IngestEvent::Published` and emit `DataEvent::Diagnostics` at Warning; or fold missing-optional into `Degraded` like missing-required.

**M10. Panics in the service's result sink kill a query worker (the containment boundary excludes delivery).**
`query/pool.rs:330-333` wraps only `run(&conn, &req)`; the sink call at `pool.rs:367-375` runs outside it, under the queue lock. The sink installed by the service contains `s.column_index("value").expect(..)` / `expect("distinct selects n")` (`service.rs:653-654`) and the tracker lock (`service.rs:669-676`). Today these are internal invariants, but the shape means any future sink bug removes a worker permanently (with `query_workers = 1`, the whole pool) with poisoned-lock recovery hiding it.
Direction: include delivery inside the worker's `catch_unwind`, or make `RequestKind::Distinct` results a typed payload so no `expect` is needed. (Noted here because the sink is service code; the pool is the other reviewer's.)

### Minor

**m1. `DataService::open` is 690 lines with ~10 copies of one closure.** `service.rs:541-1233`. The `|reported| match reported { Some((worst, detail)) => { log_health_event(..); sink(DataEvent::Health{..}) } None => true }` closure appears at `service.rs:246-258, 604-618, 741-753, 787-795, 826-838, 855-863, 940-957, 1030-1046, 1122-1136, 1190-1199` with three variants (with/without logging). Direction: `fn health_emitter(sink: &EventSink, source: &str, log: bool) -> impl FnOnce(Option<(Health,String)>) -> bool`, and split `open` into `open_store`, `seed_health`, `spawn_query_side`, `spawn_ingest_side`, `resolve_sources`.

**m2. Dead public API on `DataService`.** `freshness` (`service.rs:1643`), `as_of_bounds` (`1675`), `validate_scope` (`1274`) have no callers outside `service.rs` tests (workspace grep). `is_preemptible` (`runner.rs:718`) has no callers at all, not even tests, despite its comment "exists so the intent is testable". Direction: delete or route (`as_of_bounds` looks like what the as-of picker should be using).

**m3. Duplicated worker scaffolding across lanes.** `panic_payload_message` (`runner.rs:729-737`), `pricing/worker.rs:109-117 panic_message`, `pool.rs:403-411`; "log refused once" latches at `runner.rs:704-715`, `scheduler.rs:208-219`, `pool.rs:390-400`, `pricing/worker.rs:217-224`; `publish_one_document` and `append_one_series` (`runner.rs:322-478`) are the same containment/report shell twice. Direction: one `fn contain<T>(what: &str, f) -> Result<T, String>` and one `RefusalLatch` type in `ingest/mod.rs`.

**m4. `HealthTracker` serialises every reporter on one mutex and runs formatting + the app mailbox under it.** `service.rs:425-444, 463-487`; `log_health_event` (`198-217`) formats under the lock. Reporters: scheduler, every subscribe receiver, the ingest thread, every fetch outcome sink, and (via `load_lane`) every series result while the pool lock is held. Documented and order-consistent, so not a deadlock; but a slow `tracing` subscriber stalls ingest and query delivery. Direction: compute the decision under the lock, log and deliver after it (the tracker's "acknowledge only if delivered" needs the sink's verdict, so keep delivery inside but move `tracing` out).

**m5. Discovery does one SQL lookup and one regex compile per file per poll.** `discovery.rs:63-70,145-150` (`catalog.lookup_by_path` per candidate) and `source_config.rs:115` (`Regex::new(pattern)` inside `batch_of`, called per candidate). With a few thousand files on a share at a 5 s poll this is thousands of prepared statements per poll. Direction: one query per source (`qualify row_number() over (partition by path order by gen_id desc) = 1`) into a `HashMap<PathBuf, (size, source_time)>`; compile the regex once in `SourceSpec`.

**m6. Runner queue operations are quadratic on a cold start.** `runner.rs:225-247` (`enqueue` linear dedupe per item), `187-196` (full sort per submit), `301` (`items.remove(0)`). Fine at hundreds of files; a first-run backfill of thousands pays O(n²). Direction: `BTreeSet<(priority, Reverse(source_time), key)>` + `HashMap<key, priority>`.

**m7. Idle polling wakeups.** `runner.rs:487-490` `wait_timeout(50 ms)` and `pool.rs:322-325` `wait_timeout(20 ms)` although every state change already `notify_all`s. Harmless but 70 lock acquisitions/s while idle, on a laptop on battery. Direction: plain `wait` (shutdown notifies).

**m8. `DataService::fetch` can answer `Err` and then `Ok` for one request, and two concurrent requests duplicate adapter I/O.** `service.rs:1580-1597`: gaps are queued one by one; the first refusal answers `Err` while earlier gaps stay queued and later answer `Ok(n)`. Coverage is read from committed rows only, so two fetches of the same span before the first lands both hit the adapter (rows dedupe in `append_series`, the coverage row and the vendor call do not). Direction: a per-pair in-flight span set on the service, subtracted along with coverage; queue all gaps or none.

**m9. `subscribe` runs vendor code synchronously on the service thread during `open`.** `subscribe.rs:97-110` and `service.rs:1128-1139`. A blocking `subscribe` delays every later source and the request loop's first `recv`. Documented for shutdown, not for open. Direction: note it in the guide; long-term, subscribe on the worker thread and report `Pending` until it returns.

**m10. Channel adapter uses `.lock().unwrap()` while the rest of the crate recovers from poisoning.** `channel.rs:75,101,110,141,200,275,306,318`. No panic site exists under those locks today; inconsistent rather than wrong.

**m11. Identifiers are interpolated unvalidated; data-derived literals are escaped by hand.** Table/type names from dataset names (`ddl.rs:49`, `series.rs:23-29`, `ddl.rs:136`), column names in `"{c}"` with no `"` escaping (`load.rs:96-104`, `split.rs:108-110`, `catalog.rs:456-470`), and `geode-core`'s `validate_dataset` checks reserved names only (`schema/mod.rs:415+`). Book values from CSV rows become SQL literals with `''` escaping (`publish.rs:57-70, 98-110`). A dataset named `risk snapshot` fails at open with a DuckDB parse error rather than a config diagnostic. Direction: one identifier rule in `geode-core` (`^[a-z_][a-z0-9_]*$`) reported at `datasets.<name>`; bind partition predicates as parameters (DuckDB supports `IN (?, ?, ..)`).

**m12. Task/spec citations in comments.** `ddl.rs` (21), `store/mod.rs` (6), `series.rs` (5), `document.rs` (4), `health.rs` (4: "round 4, NEW-5", "MAJ-3 and NEW-4", "Task 3"), `panic.rs` header, `Cargo.toml` ("Phase 4b Task 2 fix round 1 (MAJ-1)"). CLAUDE.md asks comments to state the invariant, not the task.

**m13. UI vocabulary inside the data crate.** `DataEvent::Loading { path, queued }`/`LoadEnded` is the status-bar progress protocol (`service.rs:96-108`); comments reference "the status bar's progress strip", "diagnostics tile", "the picker" (`service.rs:773-777, 1003-1006`). No dependency, but the event shape is a UI contract the daemon split would have to carry.

**m14. `PlanComplete` is emitted under the ingest queue lock.** `runner.rs:471-484` — the sink (tracker + mailbox) runs while `submit_document`/`submit_series`/`submit` from every producer thread wait. Documented ("can run under the queue lock"); cheap today because the app sink is a mutex push. Direction: take the decision under the lock, emit after.

### Ideas

**I1. Daemon readiness (TODO "move ingest to background process").** What already fits: `Request`/`DataEvent` are plain enums; producers only touch `IngestHandle` + sinks; the writer is one thread; `HealthTracker` seeds itself from the catalog at open (`service.rs:566-598`), so a restarting daemon recovers degraded state. What blocks it: (1) DuckDB is single-process for a writer — a UI process cannot keep read connections open on a file another process writes and see new commits; so the daemon must own the read pool too and the UI becomes a client (Arrow IPC for `Snapshot`, keyed events over a bounded socket with refusals counted — the existing `EventSink` "false = not delivered" semantics map directly). (2) `Instant submitted` in every params struct, `Arc<Snapshot>` in `QueryOutcome`, and `Box<dyn ..>` registries in `DataServiceConfig` are not serialisable; config is already names in TOML. Suggested first step, cheap and useful now: split `DataService::open` into an ingest side (`store`, sources, adapters, documents, egress, tracker) and a query side (readers, schema, views, pool, pricing), each with its own `open`, both driven by today's `serve` — the daemon boundary then becomes one `match` on `Request`. Derive `serde` on `Request`/`DataEvent` behind a feature to see which fields resist.

**I2. Simulators as first-class fault injectors.** `ChannelAdapter` and `DemoSeries` (`geode-app/src/demo_series.rs:194-221`) go through the production traits — good. Nothing simulates the failure modes the docs warn about: a `subscribe` that blocks, a `Fetch::fetch` that never returns, an `upload` that hangs or panics, `Reconnecting`/`Lost` on a schedule. Suggest a `FaultyAdapter { delay, fail_every, panic_on, disconnect_every }` wrapper over any `Adapter` in a `geode-sim` module, registered under `--demo` flags, so shutdown-join and containment behaviour is exercised in the real app rather than only in unit tests.

**I3. Schedule retention in the runner.** Add `Work::Sweep(dataset)` at the lowest priority, enqueued every N publishes per dataset (`sweep` at `retention.rs:120-138` already owns its transaction). This bounds M5 and the archive growth `performance.md:114` records.

**I4. Make the service thread's liveness observable.** A heartbeat `DataEvent` (or `DataHandle::is_alive()` reading `JoinHandle::is_finished`) lets the shell show "data service stopped" instead of an ever-growing refusal count (pairs with C1).

## (c) Systemic patterns

- **Containment is per-worker, not per-boundary.** Workers are contained; the two threads that call foreign code from the request path (`serve`, egress) are not. The `contained` marker + crash hook design is good, but the hook's "uncontained ⇒ crash file, then continue" means an uncontained thread death is the worst of both: an alarming artifact and a silently degraded app.
- **Counters without readers.** Three refusal counters (`MessageSink`, `ChannelFeed`, `DataHandle`) are documented as the only way to see loss, and nothing reads them in production. Health lanes are the surfacing mechanism; the counters should feed them.
- **Documented limitation as a substitute for a failure signal.** M3, M4, M7, M9 are each recorded in the guide as limitations. Under "silence is a bug" a documented silence is still a silence; each has a cheap explicit-error version.
- **Per-publish cost that scales with history.** Enum refresh over the archive, persistent staging, no sweeps: individually documented, together they make a long session slower than a short one on the ingest thread.
- **Copy-paste closures over a shared helper.** The health-emit closure and the worker scaffolding are duplicated 4–10×; each copy is a place for a lane to drift (the `Failed`/`SeriesFailed` arms already differ from the others by not logging the transition).

## (d) Done well

- The two-lane `HealthTracker` (`service.rs:262-497`): decide-and-emit under one lock, per-batch load lane, change stamps to prevent equal-rank flapping, delivery-gated acknowledgement, catalog-seeded at open. The pure tests (`4435-4905`) and the service-level tests (`4907-5296`) cover the transitions the philosophy cares about.
- Bounded, refusing submission at every UI-facing door (`handle.rs:83-97`, `fetch.rs:82-92`, `egress.rs:230-244`, `pricing/worker.rs:56-74`) and the `replace_views` latest-value mailbox (`handle.rs:225-244`).
- Transactional publish: one transaction per file across grains, dictionaries and catalog (`load.rs:154-317`); ids reserved before use; the `generations` summary maintained by publish and retention with a test oracle (`ddl.rs` `assert_generations_match_tables`); the backfill guard with the "equal time is a corrected republish" rule (`publish.rs:175-186`); NULL-book partitions handled everywhere with `is not distinct from`.
- Series storage as epoch micros so no session time zone can shift a timestamp (`series.rs:14-38`), coverage recorded even for empty fetches, retention inside the append transaction.
- Every worker survives refused delivery and logs once (`runner.rs:704-715`, `scheduler.rs:208-219`, `pool.rs:390-400`), and shutdown order is stated, mirrored in field order, and idempotent.
