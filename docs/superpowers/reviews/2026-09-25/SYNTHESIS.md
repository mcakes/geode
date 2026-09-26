# Geode codebase review — 2026-09-25

Reviewed at `main` = fe548817. Fifteen parallel read-only passes (one per crate area plus
cross-cutting architecture, performance, verification, and documentation), each finding
verified against code with file:line citations. Per-area detail is in the sibling files.

## Baseline (measured, not claimed)

| Check | Result |
|---|---|
| `cargo fmt --check` | clean |
| `cargo clippy --workspace --all-targets -D warnings` | clean |
| `cargo test --workspace` | 3,666 passed, 0 failed, 1 ignored |
| `scripts/mutation-check.sh --anchors-only` | 1,575 anchors, 0 stale, 0 ambiguous, 0.19 s |
| clippy pedantic + nursery (advisory) | 4,441 warnings |

The project's own gates are green. Nothing below is a build or test failure; every finding is
something the gates cannot see.

## The five themes

### 1. Silence where the philosophy demands a signal

PHILOSOPHY §3 says "silence is a bug" and CLAUDE.md ranks a plausible wrong total above an
explicit error. The most common defect class across the workspace is a malformed-but-plausible
input that is skipped with no diagnostic, or a failure that is counted but never surfaced.

- A missing or unmounted source directory reports `Ok` on every poll (data M3).
- Three refusal counters (subscription messages, channel feed, data handle) have no production
  reader; a receiver falling behind drops snapshots silently (data M2).
- `LoadOutcome`'s extra-column and missing-optional notes never leave `load_file` (data M9).
- A saved scope with a malformed `values` array becomes an empty selection, which composes to
  "matches nothing" with no warning (core M2 + M1).
- A join whose dataset is unknown or whose key no grain carries is dropped silently; the column
  is absent from the snapshot and paints blank forever (query C2).
- A view column named as a measure that is not a measure disappears from the statement — and
  `kind` defaults to `"measure"`, so the most likely authoring mistake is the silent one (query M1).
- The picker's distinct query filters out NULL, so the NULL-book partition that the storage layer
  carefully preserves is unpickable and the counts do not sum (query M6).
- A keyboard `frame::slot_N` for a slot a reload removed does nothing; only the mouse route
  reports it (shell-interaction M1).

Several of these are already written down in the current guides as known limitations. That is an
honest house style, but for this set the documented behaviour is a defect rather than a boundary.
Worth one pass asking of each documented limitation: is this a boundary, or a bug we wrote down?

### 2. Containment is per-worker, not per-boundary

Every background worker in geode-data wraps foreign code in `catch_unwind`. Two threads that call
foreign code do not: the service request loop (`handle.rs:296-397`, zero `catch_unwind` in the
file) and the egress worker (`egress.rs`, zero). A panic on either exits the thread; the app then
refuses every query, upload and fetch forever while looking alive, and the only trace is a counter
nobody reads. The panic hook writes a crash file and returns, so an uncontained thread death is
the worst of both worlds: an alarming artifact and a silently degraded app. (data C1, M1.)

### 3. Invariants held in prose that the type system could hold

This is the dominant clarity finding, and it recurs in every area. The codebase has the technique
— `const _: () = assert!(...)`, exhaustive non-wildcard matches, single-door accessors — and
applies it brilliantly where it is applied. Where it is not, a comment instructs a future reader:

- `holds_shell_focus` lists four focusable fields with a comment saying "add the fifth here".
- `close_modal`, `sync_dialog_text` and the input subscription each hand-maintain the same
  seven-dialog chain; the compiler checks none of them.
- Restart-required config detection encodes its document list in three places.
- `session_dirty` is set at fourteen call sites; what actually makes it correct is a separate diff.
- The tiling fullscreen invariant is patched at each call site rather than owned by the setter, so
  session restore can still put a hidden stack member on screen full-window.
- Nine config readers hand-copy a five-step preamble; the tenth forgot the `config_version` skip.

### 4. Cost that grows with the session

On the ingest thread, three individually documented decisions compose into a per-message cost that
rises over a trading day: `refresh_enum` rescans live plus archive on every publish, staging tables
are persistent rather than TEMP so every payload is written twice, and document archives have no
production retention sweep. A busy feed therefore gets slower as the day goes on, and publish
latency is staleness the trader sees. (data M5, M6, M7 and performance.md's own known gaps.)

### 5. Verification is strong where data is silent, thin where a human is assumed to be looking

3,666 tests, 1,575 mutation entries every one naming a test, zero `#[ignore]`, test names that
state contracts. The gaps are specific: the 8 ms pure-UI budget has no benchmark at all; roughly
two dozen "display check pending" items live only in session memory rather than in a current guide;
and the mutation harness validates its anchors twice but never validates the test filter that makes
"caught" mean anything — one entry names a test that does not exist and still reports caught.


## Confirmed critical findings (I re-verified each in code myself)

| # | Where | What | Cheap fix? |
|---|---|---|---|
| 1 | `geode-data/src/handle.rs:296` `serve` | The service request loop has no `catch_unwind` (the file has zero). A panic exits the thread; every later query, upload and fetch is refused forever while the app looks alive. | Medium |
| 2 | `geode-data/src/egress.rs` `work` | Egress worker likewise uncontained. A transport panic breaks the "exactly one answer per upload" contract and silently drops queued jobs. | Yes |
| 3 | `geode-core/src/schema/mod.rs:219` | `SchemaSpec::from_doc` is the only top-level reader missing the `config_version` skip. A `datasets.toml` with the conventional version stamp mints a phantom dataset that reaches `apply_schema` and the dataset pickers. Nine sibling readers have the guard and most have a dedicated test; this one has neither. | One line |
| 4 | `geode-blotter` sort identity | `SortSpec.column` is a plan index. `move_column` never remaps it, and a plan rebuild only bounds-checks it. Dragging a column, hiding a column, or regrouping silently sorts by a different column, and the header arrow agrees with the lie. (Project memory lists this as deferred; it deserves promoting.) | Medium |
| 5 | `geode-data/src/query/compile.rs:686-702` | A join whose dataset is unknown or whose key no grain carries is `continue`d. The column is absent from the snapshot, so it paints blank forever with no diagnostic. `ViewSpec::validate` does not check join keys at all. | Yes |
| 6 | `geode-app/src/bridge.rs:180` + `store/mod.rs:95` | No single-instance guard on a single-writer DuckDB file. A second launch either degrades to one opaque diagnostic or contends on the file, and nothing tells the trader which. | Medium |
| 7 | `geode-app/src/main.rs:288-345` | Quit hooks race gpui's 200 ms `SHUTDOWN_TIMEOUT`. A quit during ingest abandons the join and can lose the last session snapshot. The guides describe the join as if it completes. | Medium |
| 8 | `scripts/mutation-check.sh:18019` | One mutation entry names a test that does not exist. The harness clears the filter, runs all 201 pricer tests, and still prints `caught`. No `--exact` anywhere, so eleven more entries can be judged by a different test than the one named. | Yes |

## Per-area majors worth acting on

**geode-core.** Silent fallbacks in typed readers are the dominant class: a non-table `dimensions`,
a non-array `values`, a non-string `text` are all dropped with no diagnostic, and an empty selection
composes to "matches nothing". `Scope::and_then` intersects with `Vec::contains` inside `retain`
(quadratic) and is order-dependent for unnormalized input. `record_provenance` scans the whole
provenance map per leaf, making config merge super-linear on the keystroke path — and the bench
fixture deliberately omits the largest document (the builtin keymap lives in geode-shell), so the
measured number is a known underestimate. `atomic_depth`'s whole-object list is a hardcoded table
with no link to the readers that depend on it. `scopes.rs:117` recovers a column name by splitting
its own diagnostic message on quotes.

**geode-data query compiler.** Identifiers reach SQL as `format!("\"{name}\"")` with no escaping and
no upstream character validation, so a `from = 'book" or true --'` in `dimensions.toml` yields a
predicate that is valid and silently true. A non-measure column named as a measure falls back to
`Aggregate::Sum` — a numeric attribute at a grain therefore gets summed, producing a plausible wrong
total. Derived-column reference detection compares byte-exact against case-insensitive SQL, so
`DELTA01 / NPV` matches nothing and takes the branch that hands out the *strongest* attribution
claim, against the function's own documented rule. `try_cast` on a stale ENUM makes a NULL that is
indistinguishable from a rolled-up cell. Live `Freshness::generation` reports a database-global
counter, not the dataset's (currently read by nobody, so a latent trap).

**geode-shell state layer.** The pure/gpui split is genuinely intact: no gpui import in `tiling/` or
`keymap/`. Session restore filters `fullscreen` against all stack members including hidden ones, so
a session naming a hidden member restores and paints the wrong tile full-window; the live verbs are
guarded, but by three per-call-site clears rather than one invariant. Session save can lose the last
layout change at quit (the dirty flag is cleared before the path lookup, and session writes bypass
the `config_write` FIFO that fixed exactly this class of race for config). Binding resolution is a
linear scan over every binding with a fresh `Vec<KeyContext>` per keypress, built twice on the common
path. `effective_binding` is quadratic and runs per rendered row in the keybindings dialog.
Keyboard resize refuses at the minimum while mouse drag clamps to it, so a keyboard-only user cannot
reach a layout the mouse can.

**geode-shell interaction layer.** No criticals. Keyboard `frame::slot_N` swallows a missing slot
that the mouse route reports. Escape in the scope field skips the revert when no session base was
recorded, leaving a scope session open with no owner. Every module re-implements the flip-barrier and
version-following protocol by hand, with call counts that diverge per module — the single
highest-leverage extraction available. `TileContent::deliver` forces every module to write empty arms
for deliveries it can never receive. Restart-required detection encodes its document list in three
places. `ShellView` holds twelve fields that are "one open dialog and its scroll handle", with three
parallel hand-maintained chains over the same seven states.

**geode-app.** `bridge::attach` is 480 lines whose drain `match` copies the version-guard ritual four
times — and the `Loading` arm already lost its copy, notifying unconditionally, with nothing stating
which is right. Two independent frame observers implement the same counter-watch mechanism, one of
them carrying a load-bearing gpui registration-order dependency enforced only by where the code sits.
Contained panics are logged and become no data at all: a repeatedly panicking source publishes
nothing while its health lane can still read clean. Five `ModuleFactory` forwarding wrappers are
near-identical 30-line copies that a blanket impl on `Rc<F>` would delete. No simulator exercises
reconnect, partial failure, or backpressure, so `ConnectionState::Reconnecting`/`Lost`, the fetch
timeout path and the degraded-but-queryable state are unexercised by the only sources that exist.
The demo's generated dates are frozen in August 2026, so every demo boot now shows the blotter
permanently stale — which is the fixture used for display checks.

**geode-blotter / geode-diagnostics.** Beyond the sort-identity critical: the header's grouping label
and title are committed before the submit, so a refused requery leaves the header naming a grouping
the rows are not at. `sort_siblings` expresses NULL-last as a per-pair carve-out (verified consistent,
but hard to review) and re-reads the snapshot for the descending branch, doubling comparator cost.
`render` still formats per frame in three places, including parsing an RFC 3339 timestamp per dataset
per frame. Row and chevron element ids come from the visible index while the domain row id is in hand.
Both diagnostics filter paths format every row and then discard most of them, and every filter
keystroke rebuilds the section from source data. No production-route keyboard test in either crate,
so the mode-pair key context is structurally uncovered.

**Verification.** 3,666 tests, 1,575 mutation entries each naming a test, zero `#[ignore]`, contract-
shaped test names, and negative performance tests (asserting work does *not* happen) which are rare
and valuable. The harness validates anchors twice and the test filter never: one entry names a
nonexistent test and still reports caught, and with no `--exact` eleven more can be judged by a
different test. 45 entry pairs share a (file, anchor) so the second mutates the same occurrence, and
43 pairs have one anchor as a substring of another. `--changed` selects on the mutated file, not the
guarding test file, so a weakened assertion is invisible to the everyday mode — which is where the
UTF-8 expectation-corruption incident lived. The 8 ms pure-UI budget has no bench at all. CI omits
the project's own 0.19 s merge gate.

**geode-timeseries / geode-chart / geode-widgets.** No criticals; geode-chart is genuinely pure
presentation and its rebuild counters let tests assert "an unchanged frame rebuilt nothing" as a
number, which is rare and excellent. The displayed-time zone is carried as a scalar offset snapshotted
at rebuild, so any chart whose span crosses a DST change labels gridlines and ticks at today's offset
— wrong axis labels on most charts for half the year, with correct data underneath. The stats window
is derived from the previous delivery's buckets, so a frequency change while zoomed computes
percentiles and density over the wrong span. `ChartKey` excludes `percentiles` on a rationale that is
factually wrong about the code; it stays sound only via `result_seq`, and a *refused* requery breaks
that, leaving the chart painting statistics the trader just switched off. Percentile tags are rebuilt
and re-laid-out every frame outside both caches, contradicting the documented allocation contract.
Density bars clamp out-of-domain bins to the strip edge — the exact behaviour the percentile lines
were changed to avoid, argued against in writing fifty lines earlier. The crosshair readout is the
one capability with no keyboard route, which is a §2 violation rather than a bug.

**geode-shell dialogs.** No criticals. `objectdialog/render.rs` is 4,755 lines mixing key routing, six
stage transitions, three pointer vocabularies and two render trees, with ~40 copies of the same
`&mut ShellView` → state → draft prelude. Domain behaviour is split between exhaustive ladders in
`mod.rs` and ~28 ad-hoc `domain == Domain::X` tests in `render.rs`; the latter is where a seventh
domain will be silently forgotten. `Draft::visible_rows()` re-ranks and reallocates the whole row list
4-10 times per keystroke. Four filter-only dialogs write the shared input directly, so the "one text
writer" rule holds for four of eight dialogs, and the workaround it forces ("re-feed live text before
Pick") appears in three transcriptions. `press_verb` is string-keyed with a silent `_ => {}`, so the
mouse and keyboard routes can disagree. The read-only gate on Schema is an allow-list of mutating
verbs rather than deny-by-default, so a new verb is writable until someone remembers.

**Root cause found for a TODO item.** "View editor can't drag to reorder (clicking immediately goes to
next page)" is explained: the same list row carries both `on_drag` and an `on_mouse_down` that opens
the column stage, and for a Views member row that is always a door row. So the press that starts the
drag also navigates away, leaving nothing to drop onto. The fix is to defer door-opening to mouse-up
when no drag started, or to require a double-click on rows that are also drag sources.

**geode-pricer / geode-pricing.** The leaf boundary is the best-enforced architectural rule in the
workspace: the calculation crate depends on geode-core alone, only geode-app names it, and the
prohibition is written into the manifest where a future `cargo add` must read it. One Philosophy §1
concern, which is a judgment call for the desk rather than a mechanical bug: `parse_expiry` resolves
a month code to the third Friday with no underlying in scope at all, so `Z26` means the same date for
every index. The sibling `Expiry::tenor` explicitly refuses to resolve `3m` on the grounds that "that
is the library's calendar, not ours" — a month code is arguably the same class of thing, and the
third-Friday rule is not universal across index products. The strategy leg tables (risk reversal,
butterfly, calendar) are financial definitions living in the module with zero mutation entries, which
makes them the highest-consequence least-guarded data in the crate.
Two seam leaks: a *pricing-worker* queue refusal (as opposed to a channel refusal) parks every line
in `Failed("resubmit")` with nothing that resubmits, self-healing only because the default refresh
tick is 30 s; and a free-text underlying containing `;` or `=` corrupts the packed `spot_overrides`
attribute on save. `Sheet::index_of` is a linear scan called per id inside per-edit and per-delivery
loops, so sheet-wide edits and deliveries are quadratic. The action menu computes enabled state
without the loading gate that `dispatch` enforces, so during a load it offers rows that refuse when
picked and one that silently does nothing.

**The duplication verdict, across three independent reviewers.** Market-data, timeseries and pricer
have each independently built: a popover surface (character-identical in all three, with the same two
geometry constants declared separately), an action-menu highlight stepper, a `MenuItem` enum, a
cell-editor lifecycle, and the frame-follower protocol. Every copy was re-derived correctly, and the
newest copy is usually the best-specified — which is the strongest argument for hoisting it. The
sibling-dependency rule is being honoured in spirit and violated in mechanism. `geode-widgets` exists
for exactly this and currently holds one widget.

**geode-marketdata + geode-documents.** Better than its headline size suggests: `tile.rs` is 14,729
lines but tests start at 5,406, so production is 5,405 and the 303 tests mostly drive the real
`TileContent` trait. `apply_snapshot` decides everything on a *copy* of the draft before committing,
which is the crate's best idea. No financial reasoning anywhere: CVI slice values are read, painted
and written back, and a slice whose rows disagree is refused rather than averaged.
The critical: a generation's identity is its source time alone, so a republish that keeps its source
time leaves the draft `Editing` while the grid is rebuilt underneath it. Edits are keyed by (row,
column) and the model's column order is the document's own node order, so a same-time republish that
reorders nodes re-points every edit onto a different node, paints it there, and `:upload` will send
it. The draft's label side-map would catch this but is consulted only by `rebase`. Unreachable under
`--demo`; reachable for a source configured `source_time = "document"`.
Two performance defects outside `render` and therefore easy to miss: `serialize()` runs a full
`MatrixModel::build` on the shell's 500 ms session tick, which the perf guide measures at 8.18 ms for
a 10,000-row schedule — so a dividend panel with one unsent edit spends ~8 ms of UI thread twice a
second forever, to recompute group sizes that only change when the document does. And `/` rebuilds a
whole-document text index per keystroke, allocating ~10,000 strings and copying ~50,000 cell texts per
character on that same document shape.
A crisp inconsistency worth fixing on sight: `parse_attr`'s I64 arm carries an explicit comment that
it must never round-trip through `f64` because that "loses every integer above 2^53, silently" — and
the *cell* path does exactly that. The same typed value is exact in an attribute and truncated in a
cell. Latent today because the dividend amount is F64, but a fixture for the I64 shape already exists.
Also: staleness has no invalidation source at all. The production half of the file contains no timer
and no spawn, so a document that goes stale while the app is idle keeps painting in the quiet tone
until something unrelated notifies. That is the one signal that says a number is not current.

## The architectural verdict

The declared dependency rules hold exactly, which is unusual for fourteen crates written in a month.
geode-core has no gpui, DuckDB or socket; shell and data never see each other; no feature depends on a
sibling; the calculation crate depends on geode-core and nothing else in the world, and no module names
it. Every target sets `bench = false` and every Criterion target `harness = false`. Four globals, held
deliberately, with the rule written down. Philosophy §1 holds strictly: no financial arithmetic outside
`geode-pricing`, with the pricer's expiry resolution the one thing worth a ruling.

**The coupling moved out of Cargo.toml and into the module contract.** `TileContent` has ten required
methods and provides nothing at all. So five modules each hand-roll the popup surface (character-
identical in three crates, with the same two geometry constants declared separately), the action menu
(three near-identical `MenuRow` types with three `step`/`snap` implementations), the notice slot, the
flip-barrier state machine, the refusal message (four independent copies of "busy or gone"), the stale
clock, and the vim motion keymap under five separate action namespaces — so a trader who remaps `j`
must do it five times against five action ids. Philosophy §4 says a module must never invent its own
navigation idiom; today it has to, because the shell offers paint doors and no interaction door.

Two other structural points worth the user's judgment:

- **A market-data panel costs a release, not a config file.** `PanelSpec` is entirely `&'static str`
  with `pub const CVI` and `pub const DIVIDEND` compiled in. Philosophy §5 says the opposite in as
  many words, and the blotter honours it — any views.toml entry becomes a tile. The config machinery
  already does everything a `panels.toml` would need.
- **Health reaches two of five tiles.** Grepping non-test code for health or degradation: market-data
  2, timeseries 2, blotter 0, pricer 0. The blotter and pricer show staleness but never the source
  health that would explain it, so §3's "source health is always visible" is answered one tile away.

**Errors are `String` at every UI seam** — about 170 `Result<_, String>` signatures against nine
structured error types, all in core and data. That is why the four "busy or gone" messages drifted into
four literals and why the pricer's backoff had to be built on a boolean.

**The shape of the next phase.** Of the five TODO items, the daemon split is medium-ready (the
request/outcome seam is already the only door and the delivery docs already read as if they crossed a
process boundary; what is missing is that nothing is serialisable and three trait-object registries
live on the app side). Separate windows is closer than it looks (ShellView is already per-window and
the bridge already takes a window handle; the blockers are single-window delivery routing and a session
format with no window dimension). The dialog stack is cheapest but wants the nine parallel dialog state
fields collapsed into one enum first, which also gives the back button. Shared key bindings is already
true for config and false for vocabulary. The first-open wizard has no first-run signal, and the
cheapest definition is "no sources.toml in the user or desk layer" with the wizard as the empty-dock
surface rather than a modal.

**The one lever that makes four of those cheaper is the shared tile crate**, and it is not on the TODO
list. It is also what the cross-module TODO items ("autosize columns, works on all tiles", multi-select
in two modules, a context menu) have no home for today.

## The performance verdict

No critical. Nothing stalls the UI thread and no lock is held during render or across an await; every
lock recovers from poisoning rather than panicking. All thirteen published reference numbers map to a
committed benchmark at the stated fixture shape, and fixture construction is correctly hoisted out of
every timed closure. That is rarer than it sounds and it is what makes the rest of the document
trustworthy.

**The modules learned the lesson and the shell chrome has not.** Every module tile prepares its model
outside render and pays only refcounts inside it. The status bar, the sidebar, the blotter header and
footer, and the three config dialogs still derive and format inside render. The status bar builds its
pending-keystroke string before checking whether anything is pending, so it allocates on every frame the
app ever paints. The keybindings dialog is the largest per-frame derivation in the workspace: it walks
the whole action registry, and for each action runs a binding scan that itself contains a nested scan,
then sorts twice. Current cost is tens of microseconds, so the reason to fix it is the contract and the
scaling, not today's budget — it grows as the product of registered actions and user keymap entries.

**A documentation contradiction worth settling.** The performance guide promises that config dialogs
"derive small row sets on change". The object dialog's own comment states they are "derived fresh at
every call site (render, key handling, click resolution), never cached". Both cannot be right, and the
next author will follow the comment nearest the code.

**One finding I would treat as higher than its filed severity:** the 500 ms poll loop awaits a
`read_dir` plus per-file `stat` of the desk and user config directories, forever. That is correct and
cheap on a local disk. The desk directory is a shared location by design, and Philosophy §3 names a slow
network share explicitly. On a stalled mount that await never returns, which silently stops the whole
loop — including the session flush and the flip-barrier sweep that share the iteration. A filesystem
watch, or simply moving the desk scan to its own task, keeps a hung share from taking the session with it.

**Where the columnar claim is not yet true:** series query results come back row-wise through the DuckDB
`Row` API while the main view query is Arrow-columnar end to end. And the documented "series statistics
repeat the bucketing prefix" gap is understated — the code issues a separate prepared statement per
statistic per slot, so it is O(slots × 3) round trips, and the published 14.5 ms figure is the two-slot
case against a design that allows four y-axes.

**The cheapest three wins, no design decisions needed:** `sort_by_cached_key` in the which-key sort;
an early return for the empty case in the status bar; and holding the theme name and status messages as
`SharedString` instead of re-allocating them each frame.

## Documentation and clarity

**The guides are in much better shape than the code comments.** Of roughly a hundred concrete,
checkable claims across the current guides — a constant, a table name, an ordering rule, a failure
semantic — the large majority held exactly, including every numeric constant traced and all DuckDB
table names. All 200-plus relative links and every anchor resolve. The verified drift is small and
concentrated in feature-ownership claims, and the in-flight `update-docs` branch already fixes seven
of them. Crate READMEs are running *ahead* of the guides, which is the right direction for drift to run.

Two small doc fixes worth doing on sight: the whole-object roots list in the configuration guide omits
`egress`, although the merge table includes it and a test pins it — that is the list a config author
consults before overriding a target. And the modifier-alias section documents that invalid entries are
diagnosed and skipped, while `mod = "shift"` in fact falls back to Alt silently with no diagnostic.

**`docs/modules.md` is an orphaned product wishlist sitting in the maintained docs tree**, with no index
entry and no inbound link, reading as a description of built features. Either archive it or link it as a
roadmap.

**The real documentation debt is the code comments.** 2,472 of 41,082 comment lines — 6% — cite a task
number, phase number, spec section, review-finding id or dated ruling, which CLAUDE.md explicitly
forbids. Concentrated in shell (849) and market-data (541). The substance is usually excellent and the
citation merely additive, so most of this is a mechanical sweep that keeps the invariant sentence and
drops the identifier. A minority are citation-only and already meaningless: one comment reads "NEW-2:
see the other process::exit call's own comment above", and several module headers are task ledgers
("Task 4 added chrome, Task 6 wires the palette"). Those become unreadable once the archive is the only
home for the number, which is exactly what CLAUDE.md is guarding against. This is the single largest
readability cost in the workspace and it needs no design decisions.

**Error discipline in production code is genuinely excellent** and deserves saying plainly: three bare
`unwrap` calls in 38,644 non-test lines of geode-shell, zero `todo!`, zero `unimplemented!`, zero
`panic!`, zero `dbg!`, and no `TODO`/`FIXME` comments anywhere in the workspace, with a self-enforcing
guard test that bans `eprintln!` in library code. The one place worth revisiting is `lookup_by_path` in
the catalog store, where 21 `row.get().unwrap()` calls sit in a function that already returns
`StoreError` — defensible by construction today, but the schema-drift limitation the data guide
documents turns them into a panic rather than the diagnostic the signature promises.

**File size is the standing clarity risk:** 23 files over 2,000 lines, topped by a single 124-method
`impl` block spanning 4,590 lines. The remedy is already proven inside this repo — geode-timeseries is
split four ways with no production file over 1,300 lines, and the other three tiles have the same shape
and the same available layout.

## TODO.md status, verified

Checked by grep, all zero-hit and therefore genuinely not started: autosize columns, blotter and
market-data multi-select, the blotter context menu, scope-expression suggestions, and dialog stacking.
One item is accurate and now diagnosable: typing "scope clear" matches nothing because the palette
matcher is a single in-order subsequence match over title plus category, with no per-word scoring — and
the neighbouring "right order increases score" idea is already half-built as the category-half-weight
rule. The `[pres]` badge question resolves to a chip that already exists; the open half is only whether
a `builtin` counterpart joins it.

## Suggested ordering

This is a reading of the findings, not a plan you have agreed to. Grouped by what the work buys.

**Cheap and unambiguous (each an hour or less, no design decision).** The `config_version` guard in
`SchemaSpec::from_doc`, which is one line and has nine sibling readers plus their tests to copy.
`catch_unwind` on the egress worker, shaped exactly like the fetch worker beside it. The mutation entry
naming a nonexistent test, plus `--exact` on the harness filter so the other eleven ambiguous entries
cannot lie. The `egress` row in the configuration guide's whole-object list. `sort_by_cached_key` in the
which-key sort, the status bar's empty-case early return, and holding the theme name as a
`SharedString`. Adding `--anchors-only` to CI, which costs 0.19 s and is already the project's own stated
merge gate.

**Silent-wrong-data, in the order I would take them.** The blotter's sort identity, because it is
reachable by an ordinary column drag and the header actively agrees with the wrong answer. The
market-data same-source-time republish, because the wrong number can then be uploaded upstream. The
query compiler's `Aggregate::Sum` fallback for a non-measure column and its silently dropped joins. The
market-data I64 cell round-trip, where the fix is to copy the comment already written on the attribute
path. The pricer's expiry calendar, which is a ruling for you rather than a bug for anyone else.

**Silence that should be a signal.** The service request loop's missing containment together with a
liveness signal, since the failure mode is an app that looks alive and refuses everything. A missing or
unmounted source directory reporting `Ok`. The three refusal counters that no production code reads.
Market-data staleness having no invalidation source at all.

**Then the structural lever.** The shared tile crate. It is what M1 of the architecture pass, the
flip-barrier duplication, the five vim keymaps, the three popup surfaces, the two cell editors and the
four notice slots all resolve to, and it is what the cross-module TODO items have no home for. Migrating
one module first — the pricer, whose copies are the best-specified — proves the seam without committing
the others.

**Worth deciding before the next feature, not after.** Whether a market-data panel should cost a config
file rather than a release. Whether health belongs in the blotter and pricer headers. Whether the
performance guide's caching promise or the object dialog's "never cached" comment is the one that holds.
Where credentials will live when the first vendor adapter needs them, because the obvious wrong answer
requires no design work and the right one does.
