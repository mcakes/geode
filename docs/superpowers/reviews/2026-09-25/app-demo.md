# Review: `geode-app`, `geode-demo-data`, and the builtin configuration documents

## Summary

The composition root does its job: startup order is deliberate and documented, the shell/data
boundary in `bridge.rs` is a real adapter (no `geode-data` type escapes into `geode-shell`),
and the mailbox in `events.rs` is a genuinely well-designed coalescing seam. Nothing in
`geode-app` blocks the UI thread on data work; the two long joins (data service, demo bus) are
correctly pushed onto the background executor from `on_app_quit`.
The weaknesses are concentrated elsewhere: there is no single-instance guard on a
single-writer DuckDB file, quit-time shutdown races gpui's 200 ms `SHUTDOWN_TIMEOUT`, and
`attach`'s drain loop is one 300-line `match` in which per-lane wiring is copied four times.
Comment density is the other systemic issue — several files are more task archaeology
than explanation, in direct tension with CLAUDE.md's documentation rule.
Demo simulators are first-class adapter implementations and seed-42 deterministic, but no
simulator exercises reconnect, partial failure, or backpressure, which is exactly what the
blind-written vendor shims will need.

---

## Critical

### C1. No single-instance guard on a single-writer DuckDB database

- `crates/geode-app/src/bridge.rs:180-199` (`db_path`)
- `crates/geode-data/src/store/mod.rs:95-105` (`Store::open`)
- `crates/geode-data/src/handle.rs:280-296`, `299-317` (`spawn` / `serve`)

`db_path` resolves to one fixed per-user path (`%LOCALAPPDATA%\Geode\geode.duckdb`, or
`~/Library/Application Support/Geode/geode.duckdb` on macOS). `Store::open` calls
`Connection::open` with no lock file, no advisory lock, and no probe for another live
process; `DataService::open` then takes that connection as *the* writer and hands
`try_clone` readers to the pool. DuckDB is single-writer: a second `geode` process
launched against the same path either fails to open (the whole data layer then degrades to
a single "data service failed to open" diagnostic at `handle.rs:308-315`, leaving a running
window with no data and no explanation of *why*) or, worse, opens and the two processes
contend on the same file. Nothing in the app tells the trader which happened.
**Impact:** double-clicking the app icon twice — or leaving it open on a second desktop —
silently produces a window whose numbers never arrive. On a trading desk, a pane that
looks alive and is not is the failure PHILOSOPHY.md §3 names as the one unacceptable
state ("ambiguity never is").
**Direction:** take an OS lock (a `.lock` file beside the database with an exclusive
advisory lock, held by the service thread for the process's life) before
`Connection::open`, and turn a contended lock into a first-class refusal the shell renders —
either a modal "Geode is already running" with an activate-the-other-window route, or an
explicit read-only mode. Even without the wizard from TODO.md this is the one startup
failure a trader cannot diagnose from the UI.

### C2. Quit-time shutdown is racing gpui's 200 ms `SHUTDOWN_TIMEOUT`, so session save and data-service join can both be abandoned

- `crates/geode-app/src/main.rs:288-304` (session save hook)
- `crates/geode-app/src/main.rs:306-320` (data-service shutdown hook)
- `crates/geode-app/src/main.rs:322-345` (demo-bus shutdown hook)
- `gpui-pre-0.3.5/src/app.rs:75` (`SHUTDOWN_TIMEOUT = 200ms`), `:995-1015` (`shutdown`)

gpui collects every `on_app_quit` future and blocks the foreground executor on
`join_all` for **at most `SHUTDOWN_TIMEOUT` (200 ms)**, then logs "timed out waiting on
app_will_quit" and terminates regardless. Two of the three hooks here spawn onto the
background executor — correct, per the doc comments, because
`DataHandle::shutdown` joins a thread that can be inside an in-flight `load_file` or
discovery scan (`handle.rs:104-113`, `116-125`) — but the spawned task's completion is
what the returned future represents. If ingest is mid-load, the join cannot finish in
200 ms, and the process exits with the writer thread killed mid-transaction. The session
save (`main.rs:288-304`) is synchronous inside the hook and so usually wins, but it shares
the same 200 ms budget with the other two, and `save_session` itself does file I/O
(`geode-shell/src/shell/session_io.rs:70-86`).
The docs already concede "shutdown is not a flush"
(`docs/current/request-delivery.md:47-56`), but they do not mention that the wait is
capped at 200 ms by the framework — the guide reads as though the join completes.
**Impact:** on quit during an ingest, queued ingest work is abandoned and the last session
snapshot can be lost. DuckDB's own WAL makes torn data unlikely, but the app's promise of
a best-effort clean stop is not kept.
**Direction:** order the hooks so the session save runs first and alone
(it is the one thing that must not be dropped), and make the data-service stop a two-phase
operation: close admission synchronously in the hook (cheap, bounded) and join on a
detached thread that the process can outlive, or accept the abandonment explicitly and say
so in `request-delivery.md`. Either way the 200 ms cap belongs in the guide.

---

## Major

### M1. `bridge::attach` is a 480-line function whose drain loop hand-wires each lane four different ways

- `crates/geode-app/src/bridge.rs:481-955`, with the drain `match` at `:707-950`

`attach` does eight separate jobs: describe startup sources into diagnostics
(`:497-518`), install the catalog-refresh observer (`:522-552`), install the
`ConfigReloaded` subscription (`:556-643`), install a *second, separate* frame observer
for the pricer (`:645-700`), then spawn the drain task whose `match` handles thirteen
`DataEvent` variants. Inside that `match`, four different delivery shapes are written
out longhand: `Upload`/`Query`/`Series`/`Price`/`SeriesFetched` each repeat
`shell.update(cx, |s, cx| s.deliver(Delivery::X(..), window, cx))`; `Health`/`Polled`/
`Diagnostics`/`LoadEnded` each repeat the identical
`diagnostics.update(cx, |d, cx| { let before = d.version(); …; if d.version() != before { cx.notify(); } })`
version-guard ritual (`:790-805`, `:806-817`, `:867-880`, `:918-927`); and `Loading`
(`:881-890`) silently omits that guard and notifies unconditionally.
**Impact:** the version-guard duplication is the kind of copy that eventually loses one
copy — `Loading` already has. A reader cannot tell whether the omission is deliberate
(progress always changes) or an oversight, because nothing states the rule.
**Direction:** extract `fn note<R>(d: &Entity<Diagnostics>, cx, f: impl FnOnce(&mut Diagnostics) -> R)`
that applies the version-compare-and-notify once, and a `deliver(shell, delivery, window, cx)`
helper for the five keyed-delivery arms. That turns the `match` into thirteen one-line arms
and makes an omitted notify a visible deviation. Splitting `attach` into
`attach_sources`, `attach_catalog`, `attach_reload`, `attach_pricer_reload`, and
`spawn_drain` would also let each be tested without standing up all of them.

### M2. Two independent frame observers implement the same "watch the config counter, gate on a key" mechanism

- `crates/geode-app/src/main.rs:388-424` (diagnostics factory's `set_config` refresh)
- `crates/geode-app/src/bridge.rs:645-700` (pricer's reload)

Both observe `frame.versions().config`, both keep an `Rc<Cell<u64>>` of the last seen
value, both bail when unchanged, and both then gate a second time on whether the thing
they care about actually changed (`set_config`'s cheap clone; `pricer_config_key`'s
structural compare). They exist separately because `ShellEvent::ConfigReloaded` fires
only for five named documents — a real and well-explained limitation — but the answer was
copied rather than shared.
Worse, `main.rs:388-402` documents a **load-bearing registration-order dependency**:
gpui invokes observers in registration order, so the factory refresh must be registered
before any `DiagnosticsTile`'s own frame observer, or the tile rebuilds from the previous
`Config` and never rebuilds again. That invariant is enforced only by where the code
happens to sit inside a `cx.spawn` block, and the comment says so explicitly ("not stated
anywhere gpui enforces it structurally").
**Impact:** an ordering constraint held by comment alone, plus a duplicated mechanism, in
the one crate that is supposed to own cross-layer policy. TODO.md's "separate windows"
item is precisely the refactor the comment warns would break it.
**Direction:** give the shell one `ConfigApplied`-style notification carrying the applied
`Config` (or a typed "what changed" set) that consumers subscribe to, rather than having
each consumer poll a counter and re-derive its own diff. That removes both the duplication
and the registration-order dependency.

### M3. Contained panics are logged but never become data the trader can see

- `crates/geode-app/src/crash.rs:66-88` (contained branch)
- `crates/geode-core/src/panic.rs:34-51` (`contained` / `is_contained`)

The hook correctly distinguishes a contained panic (one of the four `catch_unwind`
boundaries doing its job) from an uncontained one, and the reasoning in the doc comment is
excellent. But a contained panic produces exactly one `tracing::error!` at
`geode::shell` and nothing else — no health degradation, no diagnostic entry, no count.
CLAUDE.md's rule is "expected failures become data"; an ingest load that panicked on one
file is an expected failure whose *data* is currently a log line the trader will not read.
The architecture guide even says a contained panic "does not normally take down the
process" — but says nothing about the source whose data is now missing.
**Impact:** a repeatedly panicking source silently publishes nothing while its health lane
can still read clean (the panic aborts before a load outcome is reported). That is the
"plausible wrong totals" failure mode CLAUDE.md calls more serious than an explicit error.
**Direction:** route contained panics into the diagnostics data lane (and, where the
boundary knows its source, into the load health lane) so the diagnostics tile shows
"source X: N contained failures". The hook already has everything it needs except a sink.

### M4. `chrono::Utc::now()` in the publication-history stamp bypasses the `Clock` seam

- `crates/geode-app/src/bridge.rs:782` (`at: chrono::Utc::now()`)
- `crates/geode-app/src/main.rs:190-193` (demo bus correctly uses `Clock::from_config`)

CLAUDE.md: "Displayed times use `geode_core::clock::Clock`; do not use `chrono::Local`."
The `Publish` record's `at` is a displayed time — it feeds the frame's recent-publication
history, which the as-of dialog offers as presets. It is stamped with a raw
`chrono::Utc::now()`. This is not the `chrono::Local` the rule literally forbids, and UTC
is the right *storage* choice, so the value is not wrong today; the concern is that the
seam is bypassed, so a configured clock offset (`AppClock`, which the timeseries work
introduced precisely so an offset is observable) does not apply here. `main.rs:190-193`
shows the crate knows the rule — the demo generator's "today" goes through
`Clock::from_config` even though it never paints.
**Impact:** inconsistent time source between the two places in this crate that stamp a
trader-visible instant; a clock offset applied for testing or a simulated session will not
move publication history.
**Direction:** stamp through the same `AppClock`/`Clock` door the rest of the app uses, or
document at the call site why arrival time is deliberately host-clock and exempt.

### M5. `SystemTime::now()` appears five times in the drain loop as the diagnostic arrival stamp

- `crates/geode-app/src/bridge.rs:619`, `:691`, `:799`, `:813`, `:883`

Every diagnostics/health/loading arm calls `SystemTime::now()` independently. Since the
mailbox coalesces (`events.rs:107-170`), several of these can be for state that arrived at
quite different times, and each gets a stamp taken at *drain* time rather than arrival
time. `request-delivery.md:96` documents this for publication history ("the history
timestamp is event arrival time, not source freshness") but not for the health and
diagnostic lanes, which have the same property.
**Impact:** minor in effect, but it means a health transition's displayed time is when the
UI got around to it, and a burst that coalesced can show several unrelated events sharing
one instant. For a tool whose whole pitch is honest freshness, the distinction matters.
**Direction:** take one `SystemTime::now()` per drained event at the top of the loop and
pass it down, and state the stamp's meaning in the guide alongside the publication-history
sentence.

### M6. No simulator exercises reconnect, partial failure, or backpressure — the three things the blind vendor shims will get wrong

- `crates/geode-app/src/demo_series.rs:196-221` (`DemoFetch`: `fetch` always succeeds for a
  known identity, `catalogue` is a constant)
- `crates/geode-app/src/demo_bus.rs:126-157` (`publish_one`: the only failure path is a full
  queue, which is logged once and skipped)
- `crates/geode-data/src/adapter/mod.rs:105-135` (`ConnectionState::{Connected, Reconnecting, Lost}`,
  `HealthSink`)
- `crates/geode-app/src/demo_bus.rs` — no `HealthSink` call anywhere

The memory note "no real sources: simulators first" says every adapter shape must be built
against a first-class simulator because the vendor shim is written blind later. The
simulators are genuinely first-class as *adapters* (see W3 below), but they only ever
model the happy path. `ConnectionState::Reconnecting` and `Lost` exist in the trait, the
service maps them into the discovery health lane, and **nothing in the demo ever emits
them** — the `ChannelAdapter`'s subscribe reports `Connected` and that is the end of it.
`DemoFetch::fetch` never times out, never returns a partial span, never errors except on
an unknown identity; the trait's own doc says "implementations must bound both fetch and
catalogue calls with their own timeouts" and the demo therefore never proves the worker
survives one that does not.
**Impact:** the discovery health lane, the reconnect display, the fetch-timeout path, and
the "source degraded but still queryable" state are all unexercised by the only sources
that exist. Every one of them is a path a Solace or KDB shim will hit in week one.
**Direction:** add a fault-injection surface to the demo adapters — a config-driven
schedule of `Reconnecting`/`Lost` transitions, a fetch that sometimes returns a truncated
span or an error, a catalogue that sometimes answers `None`, and an adapter that blocks
past a deadline. Driving them from the demo config layer keeps it a config change rather
than a rebuild, and makes each failure demo-able for the display checks.

### M7. Message schema is not config-mapped: `topic_prefix` and the wire vocabulary are compiled in

- `crates/geode-app/src/demo_bus.rs:34-40` (`Producer.topic_prefix: &'static str`)
- `crates/geode-app/src/main.rs:198-216` (`"marketdata/cvi/"`, `"marketdata/dividend/"` literals)
- `crates/geode-app/src/demo.rs:50-66` (the same prefixes again, as topic patterns in the
  generated `sources` doc)
- `crates/geode-app/src/demo.rs:76-80` (and a third time, as egress addresses)

The memory note requires "message schema config-mapped". The topic namespace is
hard-coded in three places that must agree: the producer's prefix, the source's topic
pattern, and the egress address template. A test pins the source and egress copies
(`demo.rs:186-208`, `:137-183`) but nothing ties either to `main.rs`'s producer literal —
change the producer prefix and the demo silently publishes onto topics no source
subscribes to, with no diagnostic (an unmatched topic is simply never delivered).
**Impact:** a three-way invariant held by nothing. More importantly, the shape a vendor
shim needs — "this field of this message maps to this document column" — has no
configuration surface at all yet; `DocumentKind` compiles the mapping in.
**Direction:** near-term, derive the demo's topic namespace from one constant so the three
uses cannot drift. Longer-term, this is the design gap to close before the first vendor
adapter: the document kind needs a config-mapped field→column layer, and building it
against the demo bus is exactly the memory note's instruction.

### M8. The `ModuleFactory` forwarding wrappers are five near-identical 30-line copies

- `crates/geode-app/src/main.rs:561-592` (`BlotterFactoryHandle`)
- `crates/geode-app/src/main.rs:607-633` (`MarketDataFactoryHandle`)
- `crates/geode-app/src/main.rs:649-676` (`TimeseriesFactoryHandle`)
- `crates/geode-app/src/main.rs:680-707` (`PricerFactoryHandle`)
- `crates/geode-app/src/main.rs:712-742` (`DiagnosticsFactoryHandle`)

Five structs, each a newtype over `Rc<F>`, each forwarding the same five trait methods
verbatim. The comments are emphatic and correct about *why* every defaulted method must be
forwarded (`MarketDataFactoryHandle`'s kind `cvi` and context `marketdata` differ, so an
inherited default silently drops the whole fragment) — but the fix was applied by copying
the care five times rather than by removing the possibility of the error.
**Impact:** ~150 lines of pure duplication, and the failure they guard against reappears
the moment a sixth module is added by copy-paste with one method missed. The comment
admits this is what it looks like when it happens.
**Direction:** one blanket `impl<F: ModuleFactory> ModuleFactory for Rc<F>` in
`geode-shell::module` (or a `RcFactory<F>` newtype there) deletes all five. The
`BlotterFactoryHandle` doc comment's stated reason for not doing this — keeping
`geode-blotter`'s surface to one impl — does not apply to a blanket impl in the shell,
which is where the trait lives.

---

## Minor

### N1. `install_logging` reads `config_dirs()` a second time, before the config exists

- `crates/geode-app/src/main.rs:475-520` (`install_logging` calls `config_dirs()` at `:509`)
- `crates/geode-app/src/main.rs:791-800` (`build_shell_services` calls it again at `:793`)

`build_shell_services`' doc comment says "one `config_dirs()` call, one source of truth for
what's watched" — but `install_logging` already called it independently to find the log
directory, because it must run before config load. Two callers, two resolutions; they agree
only because the function is pure over the same two env vars.
**Direction:** resolve once in `main` and pass the pair to both. The comment's claim then
becomes true.

### N2. The daily log filename rolls on UTC while every displayed time is the configured clock

- `crates/geode-app/src/main.rs:462-472` (the MIN-7 note)
- `crates/geode-app/src/crash.rs:186-212` (`crash_timestamp`, deliberately UTC to match)

Documented in detail and deliberately not fixed: west of UTC, `geode.2026-09-08.log` holds
the evening of the 7th local. The reasoning (tracing-appender exposes no local-clock
rotation) is sound and the crash file's UTC stamp is consistent with it.
**Impact:** a trader asked for "today's log" picks the wrong file for part of the day.
**Direction:** leave the rotation alone; write the local date into each record's own
formatted line, or print the resolved log path at startup so "the current log" is
unambiguous.

### N3. Log files are trimmed once at startup and never again

- `crates/geode-app/src/main.rs:509-515` (`crash::trim_log_files(&logs, 7)`)
- `crates/geode-app/src/crash.rs:319-330` (`trim_log_files`)

The seven-file cap applies at startup only; `tracing-appender` rotates forward without
pruning. A process left running across weeks accumulates unbounded daily files. Crash
files, by contrast, are pruned on every write (`crash.rs:214-278`, `CRASH_FILES_KEPT`),
so the two housekeeping paths differ in a way nothing explains.
**Direction:** either prune on rotation, or state the "restart to prune" behavior beside
the constant.

### N4. Sensitive data in logs is unaddressed

- `crates/geode-app/src/main.rs:475-533` (three sinks: stderr, 4096-entry ring, daily file)
- `crates/geode-app/src/crash.rs:216-250` (the crash file embeds the whole ring)

Nothing filters what reaches the ring or the file, and the crash file writes the ring
verbatim into a file on disk beside the logs. Today's log lines are diagnostics, health,
and config paths, so the exposure is small — but the crash file is the artifact most likely
to be emailed to a developer, and it will faithfully carry whatever a future log line puts
in the ring (a scope expression naming books and counterparties already qualifies as desk
information).
**Direction:** state a rule for what may be logged at each level, and consider redacting
query/scope text from the crash file. Worth settling before the first vendor adapter logs
a connection string.

### N5. Secrets have no home in `sources.toml` / `egress.toml`, and nothing says so

- `crates/geode-core/src/source_config.rs:76-103` (`SourceSpec` fields: no credentials, no
  free-form settings map)
- `cratests/geode-core/src/egress_config.rs` (`EgressSpec`: `adapter` + `documents` only)
- `docs/current/configuration.md:214-240` (egress configuration section)

There is no `settings`/`extra` table an adapter can read, and no credential mechanism. For
the current demo adapters that is correct and clean. For Solace/KDB/Sophis/BBG it is a gap
that will be filled under time pressure, and the obvious wrong answer (a password field in
a world-readable `~/.config/geode/sources.toml` that the app also *writes*) is the one that
requires no design work.
**Direction:** decide now that credentials come from the environment or an OS keychain and
never from a layered config document, and write that into `configuration.md` so the
decision is made before the shim author needs it.

### N6. Uploads have no retry and no idempotency key

- `crates/geode-data/src/egress.rs:139-157` (`work`: one transport call, result answered, no retry)
- `crates/geode-data/src/egress.rs:204-259` (`upload`: refusals answered synchronously)
- `docs/current/data-path.md:188-229`

Strictly in scope as the app's egress story: every upload is exactly one attempt, and the
outcome is a `Result<(), String>` the tile shows. There is no retry, and — more
importantly — no idempotency token, so a user who retries by hand after an ambiguous
failure (transport error *after* the far side committed) sends a second copy. The echo
compare the demo uses is a demo affordance, not a delivery guarantee.
The single-attempt design is defensible (the trader decides), and the docs are honest about
it. The missing piece is the token that makes a manual retry safe.
**Direction:** carry a per-upload idempotency key (document key + revision + tag) in the
address or payload so a real target can dedupe, before the first non-demo egress adapter.

### N7. `--demo`'s generated business dates and source times are frozen in August 2026

- `crates/geode-demo-data/src/generate.rs:72` (`format!("2026-08-{:02}", 24 + date_idx)`)
- `crates/geode-demo-data/src/emit.rs:211-215` (`as_of` = `2026-08-30T..`)
- `crates/geode-blotter/src/tile.rs:1381-1391` (`is_stale`, default 15 m)
- `crates/geode-app/src/demo_series.rs:66-76` (`ANCHOR = 2026-01-05`, and the note that the
  walk is anchored there so values look current)

The risk generator's dates and the emitted sentinels' `as_of` are hard-coded calendar
dates. Against a 15-minute `stale_after`, every demo boot after 2026-08-30 shows the
blotter permanently stale. `demo_series.rs` explicitly solved the analogous problem for the
timeseries walk (the `ANCHOR` constant exists precisely so a trader does not see six years
of compounded drift), so the crate knows the shape of the fix.
**Impact:** the demo — the thing used for every display check — misrepresents the freshness
UI, and a genuinely broken freshness reading would look normal.
**Direction:** generate dates relative to `Clock::today()` (the same door `main.rs:190-193`
already uses for the demo bus), and stagger sentinel `as_of` backwards from now.

### N8. `2026-08` dates and a fixed `2026-09-12` anchor make several demo tests date-fragile

- `crates/geode-app/src/demo_series.rs:66-76` (`EPOCH` 2020-01-06, `ANCHOR` 2026-01-05)
- `crates/geode-app/src/demo_series.rs:226-250` (the pinned-bars characterisation test)
- `crates/geode-app/src/demo_bus.rs:246-260` (test anchor `2026-09-12`)

The characterisation test is well-labelled as one ("a change here means the generator
changed"), which is the right call. The concern is narrower: `open_level` walks one step
per weekday from `EPOCH` to the requested day (`demo_series.rs:122-136`), so the cost of
generating a bar grows linearly with wall-clock time forever. By 2030 every `--demo`
fetch pays ~1,300 extra steps per identity per day.
**Direction:** memoize the daily level per `(identity, day)` or re-anchor periodically; the
`ANCHOR` mechanism already shows how.

### N9. `field_value`'s `"__usd"` suffix does not match the `"_usd"` the schema declares

- `crates/geode-demo-data/src/emit.rs:285-287` (`canonical.strip_suffix("__usd")`)
- `examples/demo-config/datasets.toml` (declares `delta01_usd` etc., single underscore)
- `crates/geode-demo-data/src/emit.rs:51-71` (`USD_TWINS`)

`field_value` strips a **double**-underscore suffix. Since the emitted headers are built by
`header_columns`/`canonical_columns` (`emit.rs:251-283`), whichever spelling those produce
is what reaches here, and the round-trip tests pass — so this is consistent internally.
But `datasets.toml` and every view spell the column `delta01_usd`, so a reader comparing
the two files sees a mismatch that is only resolved by tracing the generator. **UNVERIFIED**
whether `canonical_columns` emits `__usd` deliberately as an internal marker; confirming
means reading `emit.rs:251-283` in full.
**Direction:** if `__usd` is an internal sentinel, name it as one (`USD_TWIN_MARKER`) with
a comment; if it is incidental, unify on the single underscore the schema uses.

### N10. `RiskBatch::measure` / `identity` panic on an unknown column name

- `crates/geode-demo-data/src/model.rs:106-130` (`measure`: `other => panic!`)
- `crates/geode-demo-data/src/model.rs:133-148` (`identity`: same)
- `crates/geode-demo-data/src/emit.rs:74-80` (`source_name`: `unwrap_or_else(|| panic!(..))`)

Three panics reachable from a mismatch between the column-name constants and the struct
fields. All three are compile-time-ish invariants in practice (the constants and the match
arms are in the same file), and a panic in a generator is a developer error, not a runtime
failure — so this is a defensible choice. Noting it because the emitter is also reachable
from the `emit` example and the benches, where a bad name from a future config-driven
column list would abort rather than diagnose.
**Direction:** fine as is; if the generator ever takes its column list from config, these
become `Result`.

### N11. Task numbers, spec sections, and review-round labels dominate the comments

- `crates/geode-app/src/main.rs` — 65 occurrences of `spec §` / `Task N` / `Phase N` /
  `fix round` / `MIN-n` / `MAJ-n` / `NEW-n` / `planning decision`
- `crates/geode-app/src/demo.rs` — 20; `crates/geode-app/src/crash.rs` — 17;
  `crates/geode-demo-data/src/documents.rs` — 15; `crates/geode-app/src/demo_bus.rs` — 11
- CLAUDE.md: "A code comment should state the local invariant and failure it prevents; it
  should not require a task number or spec section to make sense."

Several comments are review transcripts rather than explanations. `main.rs:62-72` spends
eleven lines on why a `drop` call exists, labelled `NEW-2 (final review round 2)`;
`main.rs:365-402` is a 38-line paragraph containing genuinely load-bearing information
(the observer-ordering invariant, M2 above) buried in citations to `fix round 1 MAJ-3` and
`MAJ-6`. `crash.rs:118-152` is a 35-line comment on a single `tracing::error!`.
The underlying content is usually excellent and the invariants are real — the problem is
signal-to-noise and the explicit CLAUDE.md rule against exactly this.
**Impact:** the most important comment in `main.rs` (observer registration order) is the
hardest to find. New readers cannot tell which paragraphs are current contract and which
are settled history.
**Direction:** keep the invariant and the failure it prevents; move the round-by-round
narrative to the archive. `demo.rs:397-431`, where a comment about `cvi_params` was
copy-edited into saying "[dividend] (a subscribed source over cvi_params, Task 10),
[dividend] (a subscribed source over dividend_schedule, Task 11)" — naming `[dividend]`
twice and the wrong dataset once — is what happens when comments carry this much
chronology.

### N12. `demo.rs`'s `sources` document is assembled by string formatting

- `crates/geode-app/src/demo.rs:49-66` (one `format!` producing five source tables)
- `crates/geode-app/src/demo.rs:70-80` (the `egress` doc, likewise)

The demo config layer's most structurally interesting document is a single escaped format
string with `{{` doubling and an embedded regex. It parses (a test proves it) but it is the
one config document in the codebase that cannot be read as TOML.
**Direction:** build a `toml::Table` programmatically, or `include_str!` a template with one
`{source_dir}` substitution. Philosophy §5's "diffable, hand-editable" applies to the
demo's own config too.

### N13. Egress target/document pairs are flattened to `(String, Vec<String>)` for the factories

- `crates/geode-app/src/bridge.rs:352-368` (`egress_targets: Arc<Vec<(String, Vec<String>)>>`)
- `crates/geode-app/src/bridge.rs:398-421` (handed to both market-data factories)

A tuple-of-vec with a four-line comment explaining what each position means, created to
avoid `geode-marketdata` depending on `geode-core::egress_config`. The boundary instinct is
right; the vocabulary is not (the guides' "vocabulary is part of the API").
**Direction:** a named `EgressTargets` type with `targets_for(document) -> &[String]`, in
`geode-core` beside `EgressSpec`. Both factories already do that narrowing by hand.

### N14. `source_shapes` clones every `SourceSpec` to pair it with its shape

- `crates/geode-app/src/bridge.rs:326-332`
- used once at `crates/geode-app/src/bridge.rs:497-518` to build `SourceSummary` values

A full clone of every source spec, retained on `Bridge` for the life of the process, to
produce a diagnostics summary once at attach. Startup-only and small (sources number in the
tens), so the cost is irrelevant — but the retained `Vec<(SourceSpec, SourceShape)>` on
`Bridge` outlives its single use.
**Direction:** build the summaries in `start` and retain those instead; the specs
themselves are not needed after attach.

### N15. `parse_args` accepts no `--help` and no `--version`

- `crates/geode-app/src/main.rs:536-559` (`parse_args`, `usage`)

`geode --help` is an "unrecognised arguments" error that exits 2 after printing the usage
line to the *log* (`main.rs:62-76` routes it through `tracing::error!`, which reaches
stderr via the fmt layer). Exiting non-zero on `--help` is a small CLI wart; routing usage
through the logging subscriber means it is subject to `[log]` levels.
**Direction:** handle `--help`/`-h`/`--version` explicitly with exit 0 and a direct
`println!`. Note the crate's own test forbids `eprintln!` outside tests
(`main.rs:1348-1377`), so the usage path deliberately uses tracing — worth an exemption for
argument handling, which runs before any config could set a level.

### N16. `demo_series` walks from `EPOCH` on every bar request, with no cache

- `crates/geode-app/src/demo_series.rs:122-136` (`open_level`)
- `crates/geode-app/src/demo_series.rs:139-172` (`bars` calls it once per day in range)

Each requested day pays one `step()` per weekday since 2020-01-06 (~1,750 today, growing).
A one-year daily request therefore pays ~250 × 1,750 ≈ 440k twelve-uniform draws. The
`to - 1µs` optimisation at `:150-153` shows the cost was noticed for the boundary case but
not for the general one. Demo-only and off the UI thread (fetch runs on a worker), so no
budget is breached.
**Direction:** cache `(identity, day) -> open_level`, or walk forward once across the
requested range instead of restarting per day.

### N17. `assets.rs` lists exactly one extra icon, with a hard rule and no enforcement

- `crates/geode-app/src/assets.rs:22-30` (`ExtraIcons`, containing only `Save`)
- `crates/geode-app/src/assets.rs:53-78` (tests pin `Save` and the default bundle)

The module doc states the rule clearly: every catalogue icon a crate paints must be listed
here, and an unlisted one renders as an empty glyph rather than panicking. The two tests
pin `Save` specifically — they cannot catch a *new* unlisted icon named by a shell surface.
**Impact:** a module naming a catalogue icon outside the 101-icon default bundle paints
nothing, silently, and no test fails.
**Direction:** a test that greps the workspace for `IconName::` constructions and asserts
each resolves through `AppAssets` would turn the stated rule into an enforced one. (The
crate already does workspace-source scanning for the `eprintln!` rule at
`main.rs:1348-1463`, so the machinery exists.)

### N18. `profiling` gates two palette actions but the overlay binding ships unconditionally

- `crates/geode-shell/src/defaults.rs:83` (`"mod+shift+p" = "perf::toggle_overlay"`, in
  `BUILTIN_KEYMAP`, ungated)
- `crates/geode-shell/src/defaults.rs:238` (`perf::toggle_overlay` registered unconditionally)
- `crates/geode-shell/src/defaults.rs:322-344` (`perf::gpui_overlay` / `perf::dump` registered
  only `#[cfg(feature = "profiling")]`)
- `crates/geode-app/Cargo.toml:12-18` (`profiling = ["geode-shell/profiling"]`)

Consistent and correct as written — `perf::toggle_overlay` is Geode's own frame-time
readout (always available), while the two gated actions are gpui's profiler. Flagged only
because the naming makes the split easy to misread: three `perf::*` actions, two gated and
one not, distinguished by nothing in the name. Verified that the ungated binding resolves
to a registered action in every build, so the keymap test
(`main.rs:1282-1347`) genuinely covers it.
**Direction:** rename the gated pair to `profiler::*`, or say in one line at the
registration site that `toggle_overlay` is the shell's own readout and not the feature's.

---

## Ideas

### I1. Moving ingest to a background daemon: the bridge is ready, the composition root is not

TODO.md's first two items. The seam is in better shape than expected:
`DataHandle` is already a bounded, non-blocking, `try_send`-based interface
(`geode-data/src/handle.rs:280-296`) whose every method already answers "admitted" or
"refused", and the mailbox (`events.rs`) already assumes events arrive from another thread
with no ordering guarantee and coalesce by key. Neither contract would change if the other
side became a socket instead of a thread. The event payloads, however, are in-process Rust
values — `DataEvent::Query` carries an Arrow snapshot, and `EventSink` is
`Arc<dyn Fn(DataEvent) -> bool>` (`bridge.rs:277-293`). A daemon needs those serialised or
shared out-of-band.
**Readiness:** the request side is close to ready; the delivery side needs a wire format
and a decision about whether snapshots move by shared memory or by copy. The single-writer
lock (C1) becomes *easier* in this world — the daemon owns the database and the lock — so
C1 and this item want to be designed together. Separate control of ingestion data
(TODO's second item) then falls out: the daemon is the thing that can be told to pause,
resume, or reload sources without restarting the UI, which is the current
`RestartRequired` story's real fix.

### I2. First-open wizard: `build_shell_services` already computes everything it needs

TODO.md's third item. `build_shell_services` (`main.rs:791-1022`) already establishes
every fact a wizard would ask about: whether a user config dir exists
(`config_dirs()`, `:793`), whether `datasets`/`views` are configured at all
(`data_setup` returns `None` without them, `bridge.rs:64-73`), whether the roster ends up
with any data-backed module (`:884-925`), and whether the session file exists
(`:996`). The "no bridge → no blotter factory → placeholder tile" path
(`main.rs:884-892`) is already a coherent empty state.
**Readiness:** good. The wizard is a shell surface that writes the user layer through
`config_write` (already the only sanctioned write path) plus a startup branch on
"no user config dir and no desk config". The one real design question is whether
the wizard can avoid a restart — since sources and datasets are restart-required
(`configuration.md:245-259`), a wizard that configures a source must either restart the
service or accept that its first run ends in "restart to load data", which is a poor first
impression. That argues for doing I1 first.

### I3. Separate windows: one concrete blocker, and it is documented in a comment

TODO.md's fifth item. Two things assume exactly one window.
`main.rs:289-301`'s quit hook iterates `cx.windows()` and saves the session from
whichever downcasts to `ShellView` — with two windows that saves one and silently drops the
other (or saves both to the same path, last writer winning).
`bridge::attach` takes a single `WindowHandle<Root>` (`bridge.rs:481`) and routes every
keyed delivery into it, so a second window's tiles would receive nothing.
And the observer-registration-order invariant at `main.rs:388-402` explicitly names
"a second window, a different construction order" as what would break it.
**Readiness:** weakest of the four. Needs a session format with per-window layout, a
delivery router keyed by (window, tile) rather than tile alone, and the M2 refactor to
remove the ordering dependency. The `QueryKey`-based routing is otherwise window-agnostic,
which is the good news.

### I4. `events.rs` is the reusable mechanism the per-lane wiring should be built on

Not from TODO.md. `events.rs` solves "coalesce keyed state, retain the highest tag, wake
once" generically and correctly, with the fetch-success/failure asymmetry
(`events.rs:107-118`) and the publication book union (`:133-147`) as principled special
cases. The four-times-copied version-guard ritual in `attach` (M1) is a *second*, weaker
coalescing mechanism layered on top — "only notify if the version moved" is the same idea
expressed per call site. Unifying them (the mailbox already knows what changed; the
diagnostics entity should not have to re-derive it from a version counter) would remove
both the duplication and the `Loading` inconsistency.

---

## Systemic patterns

**Correct mechanism, applied by copy.** M1 (four delivery shapes, four version guards),
M2 (two frame observers), M8 (five factory wrappers), M7 (three topic-prefix literals) are
the same pattern: a genuinely correct solution, reached carefully, then duplicated instead
of extracted. The comments frequently *explain* the duplication rather than removing it —
`BlotterFactoryHandle::contexts` even names the failure that copying invites. This is the
mechanism-vs-instance failure mode already recorded in the project's own memory
("a reviewer's mechanism finding must be ruled on at every site it reaches").

**Comments as review transcript.** N11 quantifies it: 65 task/spec/round citations in
`main.rs` alone. The highest-value comments in the crate — the observer-ordering invariant,
the `WorkerGuard` contract, the panic-hook containment rule — are genuinely excellent and
are each embedded in a paragraph of round-by-round history that makes them hard to find.
This is a direct CLAUDE.md violation and the single biggest readability cost in the crate.

**Startup failures degrade well; startup *conflicts* do not.** Every invalid config
document, unknown adapter, unknown pricer, and missing directory produces a diagnostic and
a working window — this is done consistently and well (see W2). But the failures that
involve *another process or another instance* — a locked database (C1), a quit that cannot
finish in 200 ms (C2) — have no representation at all. The crate's failure model is
thorough within one process and blank at its edges.

**Demo fidelity is structural, not behavioural.** The simulators use the real traits, the
real wire format, the real coalescer, the real ingest path (W3) — the *structure* is
honest. What they never simulate is anything going wrong (M6) or time passing (N7). So the
demo proves the happy path end-to-end and proves nothing about the degraded states the
health lanes, freshness markers, and reconnect displays exist to show.

---

## What is done well

**W1. Startup order is deliberate, and its reasons are recorded.** Logging before
everything (so config load itself logs), `gpui_component::init` before any component use,
`fonts::register` after init but before the window so the first frame carries them, per-module
`init` reclaims before the window opens, panic hook after the registry exists because it
needs `hash_names()`, theme before session restore. Every step's placement has a stated
reason, and `Root` wraps the view exactly as the bootstrap guide requires
(`main.rs:355-364`). The `_log_guard` discipline — including the two `drop(_log_guard)`
calls before `process::exit` so the appender flushes (`main.rs:66-76`, `:88-92`) — is the
kind of detail that is normally discovered in production.

**W2. Invalid configuration never stops the app, and the degradation is legible.** A
missing `datasets`/`views` document yields no bridge, hence no blotter factory, hence no
"Blotter: Split" palette row, hence a placeholder tile — a coherent chain rather than a
crash (`main.rs:876-892`). An unavailable pricer installs a `PricerConfig::missing` that
fails each line with a message naming the pricers the binary *does* have
(`bridge.rs:107-128`). An unregistered egress adapter drops one target and keeps the rest
(`egress.rs:57-82`). Session-restore warnings log and startup continues
(`main.rs:277-287`). This is "expected failures become data" applied consistently.

**W3. The demo simulators are first-class adapter implementations.** `DemoSeries`
implements the real `Adapter`/`Fetch` traits with a real catalogue-vs-no-catalogue split so
both picker paths are exercised (`demo_series.rs:176-221`); the demo bus writes through
`DocumentKind::write` and publishes onto a `ChannelAdapter` so the messages traverse the
same receiver, parser, coalescer, and ingest path a real subscription would
(`demo_bus.rs:126-157`); the demo's egress target is the *same* adapter its sources
subscribe to, so an upload echoes back through the inbound path
(`demo.rs:70-80`). Nothing is stubbed at the seam. The seed-42 determinism is real and
tested (`lib.rs:19-30`), and span-independence in `demo_series` — a narrow request returning
exactly what a wide one would inside the same window (`demo_series.rs:16-21`) — is a
genuinely subtle property to have got right, and it is the property the service's coverage
subtraction depends on.

**W4. `events.rs` is the best-designed file in scope.** Per-lane coalescing rules that each
encode a real requirement: highest-tag-wins for tagged outcomes, but `(tile, tag)` keying
for uploads because each upload is a separate user action owed its own answer
(`events.rs:26-33`); publication unions affected books and keeps the greatest generation so
a superseding publish cannot lose an earlier one's invalidation (`:133-147`); a fetch
success clears a prior failure but a later failure retains the success, because the
success carries a requery signal (`:107-118`); displaced Arrow snapshots dropped after
unlocking to reduce contention (`:167-170`). Each rule has a one-line reason and a test.
The `an_idle_receiver_is_woken_by_the_final_event` test drives a real `Waker` to prove the
wakeup cannot be missed (`:186-208`) — testing the mechanism, not the mock.

**W5. The crash report is built for the person reading it.** Millisecond-resolution
`create_new` filenames so two close panics cannot truncate each other, zero-padded
suffixes so a plain name sort is a true age order (`crash.rs:214-278`), each log line
carrying its own timestamp *and* ring sequence number so the tail can be aligned against
the daily log (`crash.rs:280-297`), `try_lock` rather than a poison-recovering `lock()`
because a hook must never block on a mutex the panicking thread may already hold
(`crash.rs:99-116`), and the file written *before* the re-entrant `tracing::error!` so the
artifact survives a panic inside the subscriber (`crash.rs:118-152`). The
contained-vs-uncontained distinction (`crash.rs:66-88` with
`geode-core/src/panic.rs:34-51`) correctly recognises that a process panic hook fires
before `catch_unwind` can catch anything, and uses a thread-local *depth* counter so
nested boundaries compose.

**W6. The demo fixtures are built to exercise specific correctness contracts.** The
emitter deliberately splits `BK000` across two files *on position boundaries* because the
halves are different partitions and a mid-position split would double-count
`daily_trading_pnl` — with a test that proves no position spans two files
(`emit.rs:120-152`, `lib.rs:236-269`). It plants an attribute conflict on *some* rows of
*one* instrument, because a whole-file rewrite would leave the file internally consistent
and invisible to the grain-group detector (`emit.rs:171-200`). It withholds exactly one
sentinel to exercise pending readiness, and omits optional columns from every third file.
Coarse-grain measures repeat exactly within their grain, which is what the ingest grain
split is tested against (`generate.rs:1-4`, `lib.rs:78-120`). These are fixtures designed
by someone who knew which bug each one catches.

**W7. The keymap is verified end-to-end through the production assembly.**
`the_whole_production_keymap_builds_with_no_diagnostics` (`main.rs:1282-1347`) builds the
registry in `run`'s exact order, constructs the roster through the *real* `bridge::start`
rather than a hand-kept list, splices the real module fragments, and asserts zero
diagnostics — so a module added to `run` and not to the test is invisible, which is the
failure it exists to prevent. Cross-checking the shipped bindings confirms the discipline
holds: no duplicate key within a context across `BUILTIN_KEYMAP` and the five module
fragments, module verbs consistently namespaced (`blotter::`, `marketdata::`, `pricer::`,
`timeseries::`, `diagnostics::`), and vim idioms used identically across modules (`g g`,
`shift+g`, `ctrl+d`/`ctrl+u`, `z o`/`z c`/`z a`, `d d`, `y y`/`y c`). The timeseries
fragment's comment explaining why `+` cannot be bound at all — `parse_keystroke` splits on
`+`, and both platforms deliver shift+punctuation with shift cleared, verified against the
pinned platform sources (`geode-timeseries/src/content.rs:100-108`) — is exactly the kind
of platform-boundary note the guides ask for.

**W8. Real constraints are documented rather than quietly worked around.** The
`--demo` schema-change rule (delete `$TMPDIR/geode-demo/<rows>-42/`) appears in CLAUDE.md,
the crate README, `demo_series.rs`'s module doc, *and* as a 12-line warning inside
`examples/demo-config/datasets.toml` — because `apply_schema` is
`CREATE TABLE IF NOT EXISTS` and a grown column set fails every publish
(`geode-data/src/store/mod.rs:125-135`). The UTC-log-filename trade (N2) is recorded with
its reason rather than silently accepted. `docs/current/request-delivery.md` states what
acceptance does *not* mean at every seam ("admission, not successful execution";
"retained for delivery, not applied to a window"; "acceptance does not acknowledge
validation or application"). That precision is rare and it is what makes reviewing this
crate possible at all.

**W9. Bench and target hygiene is complete.** Every target opts out of the libtest harness
as the workspace invariant requires: `geode-app`'s `[[bin]]` sets `bench = false`
(`Cargo.toml:7-10`), `geode-demo-data`'s `[lib]` and `[[example]]` both do
(`Cargo.toml:16-17`, `:43-46`), and both Criterion benches set `harness = false`
(`:35-41`). Release and bench profiles keep debug symbols (root `Cargo.toml:81-88`). The
dev-dependency feature-parity comment — that `cargo test -p` must request `test-support` on
`geode-core` or it builds a second copy of everything above it, with the
`cargo tree -i` command to verify — is reproduced in both manifests.
