# Phase 4b — Diagnostics Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Geode observable from inside: every `eprintln!` becomes a
`tracing` event with a target and a level, kept in a ring the shell can
read and in daily files; a shell-owned `Diagnostics` entity gathers
source health, dataset generations, config diagnostics and dropped
events; a `diagnostics` module tile shows five sections over it; a
`Request::Catalog` answers what the database holds; panics in ingest are
contained per file and every panic leaves a crash file with the log tail
and the last actions dispatched.

**Architecture:** `geode-core` gains `log` — a fixed-capacity `Ring` of
`Record`s, the `RingLayer` that feeds it, and the pure `[log]` reader —
plus the `Catalog` request/outcome types both ends share. `geode-data`
answers `Request::Catalog` on its service thread from the catalog, the
`generations` table and DuckDB's own introspection functions, and gains a
`Polled` event so the shell can show last/next poll. `geode-shell` owns
the `Diagnostics` entity beside `Frame`, feeds the status bar from it,
opens module tiles by kind, keeps the last 32 dispatched actions in an
allocation-free tail, and persists `:level` through the same
`take_pending_*` drain the frame uses. `geode-diagnostics` is a new
module crate: one tile, five sections, `uniform_list` rows in the mono
face, rebuilt only when an observed version changes. `geode-app`
installs the subscriber before anything else, the panic hook after it,
and routes `Polled`/`Catalog` into the entity.

**Tech Stack:** Rust 2024, gpui (`uniform_list`), gpui-component
`0e2fb7a`, `tracing 0.1`, `tracing-subscriber 0.3` (`registry`, `fmt`,
`reload`, `filter::Targets`), `tracing-appender 0.2` (`rolling::daily`),
DuckDB 1.10505 (`pragma_database_size()`, `duckdb_tables()`,
`duckdb_memory()`), `toml_edit`, `chrono`, `criterion`.

**Spec:** `docs/superpowers/specs/2026-09-06-geode-phase-4-frame-features-design.md`
§1.1 (4b), §1.2, §4 (all of it), §6, §7 ("Other contracts"), §8, §9,
§10 (4b steps 1–6), §11 open question 2. Phase 4a and its follow-ups
(the as-of baseline, the dictionary cache, the ingest queue dedupe, the
`generations` summary table — all on `main` at `a053767`) are
prerequisites and are consumed as-is.

## Global Constraints

- **Layering:** `shell` and `data` never depend on each other; both
  depend on `core`. `geode-diagnostics` depends on `shell`, `data` and
  `core`, never on `geode-blotter`. `geode-app` is the only place they
  meet. No crate but `geode-data` opens a socket. Files: `geode-app`
  installs the log file layer and writes the crash file (the standing
  exception for config, session, now logs); the module writes nothing —
  `:level` persists through the shell's drain, exactly as slot saves do.
- **CI:** `cargo fmt --check`, `cargo clippy --workspace --all-targets
  -- -D warnings`, `cargo test --workspace`, `cargo bench --workspace
  --no-run`, and `cargo check -p geode-shell --features test-support
  --all-targets`, on macOS and Windows. Every task ends green. The log
  file path and the crash file path must build on Windows (`PathBuf`
  joins, no `/` literals).
- **Per-frame heap churn is a defect.** Code on the UI thread emits at
  `warn` or above only. Every diagnostics section rebuilds only on a
  version change. The ring's reader allocates nothing on a hit and
  `clone`s only records newer than its `since`. The action tail is a
  fixed array written without allocation. The frame histogram is copied
  into `Diagnostics` at most once per reload tick and only while a
  diagnostics tile is visible (open question 2, resolved below).
- **Nothing stalls the render thread.** The `:level` write happens on
  the background executor via `take_pending_level_persist`; the ring's
  mutex is held for a copy, never for formatting; the crash hook runs
  on the panicking thread.
- **Spec §4.5's verification rule:** every DuckDB function the catalog
  reads was run on 2026-09-08 against DuckDB 1.10505 before this plan
  was written; the exact columns are in Task 3. Do not assume others.
- **Mutation harness:** commit before you mutate; every behaviour a task
  changes gets an entry with the 6th-argument test filter; the
  controller runs `zsh scripts/mutation-check.sh --changed=<task base>`
  after every task and one unfiltered run at the end of the branch,
  detached. Implementers verify each entry by hand and never run the
  script.
- **Every new target carries `bench = false`** (`geode-diagnostics`'s
  lib; no bench target is planned).
- Commit trailers on every commit:
  ```
  Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_013f4ftJp6GLNLTj3EBs7XFL
  ```

## What already exists (do not rebuild)

- `geode_core::config::{Config, Diagnostic, Severity, Layer, LayerDoc,
  MergedDoc}`; `Config::{doc, layered_docs, get, explain}`.
- `geode_core::query::{QueryKey, AsOf, DistinctParams, DistinctOutcome}`
  — the `Distinct` pair is the model for the `Catalog` pair.
- `geode_data::handle::{DataHandle, Request}` (`query`, `cancel`,
  `distinct`, `replace_views`, `dropped_requests`, `for_tests`);
  `DataService::{query, distinct, freshness}`; `DataEvent::{Query,
  Distinct, Published, Health, Diagnostics}`; `EventSink`.
- `geode_data::store::{Catalog, FileGeneration}`;
  `Catalog::{book_freshness, dataset_as_of, lookup_by_path}`; the
  `generations` table and `resolve_generations(conn, dataset, at)`.
- `geode_data::health::Health` (`Ok`, `Pending`, `PendingTooLong`,
  `Degraded { reason }`, `Failed { reason }`, `label()`).
- `geode_data::ingest::runner` — the per-file `catch_unwind` around
  `load_file` and around the pop-time catalog re-check already exist;
  a panic becomes `IngestEvent::Failed { reason: "ingest task panicked" }`.
- `geode_data::ingest::scheduler::SchedulerEvent::{Polled, Health}`.
- `geode_shell::module::{ModuleFactory, TileContent, TileOccupant,
  ModuleRoster, FindEvent}`; `geode_blotter::content::BlotterFactory`
  and `BlotterContent` as the model for the new module's factory.
- `geode_shell::frame::{Frame, FrameVersions}` — the entity pattern
  (`versions()`, `cx.notify()`, `take_pending_persist`,
  `persist_slot_to_user_config`) the `Diagnostics` entity copies.
- `geode_shell::perf::{FrameHistogram, RequeryStats, format_ms}`;
  `shell::perf_overlay::render`; `ShellView::perf` (the histogram
  recorded at the top of every render) and `perf_overlay: bool`.
- `geode_shell::shell::status::status_bar` and `ShellView::
  {data_status, set_data_status}` (deleted in Task 4).
- `geode_shell::shell::hot_reload::apply_reload` and its `changed(doc)`
  closure; `ShellServices`.
- `geode_shell::defaults::{register_builtin_actions, BUILTIN_KEYMAP,
  register_pick_actions}`; `ShellView::dispatch` in `shell/input.rs`;
  `KeyContext::new(name).pair(k, v).counts()`.
- `geode_shell::theme::write_atomic` and the `toml_edit`
  format-preserving pattern in `frame::persist_slot_to_user_config`.
- `geode_app::bridge::{attach, start, data_setup}`; `main.rs`'s
  `config_dirs()` / `user_config_dir()`.
- `geode_blotter::tile` tests' `open(cx)` harness (`TestAppContext`,
  `DataHandle::for_tests`, `VisualTestContext`, `window.draw`) — the
  model for the diagnostics tile's tests.

## File structure

```
crates/geode-core/src/log/mod.rs        NEW: Record, Level (re-export tracing::Level), Ring, RingLayer, LogLevels, LevelControl
crates/geode-core/src/query.rs          + CatalogParams, CatalogOutcome, CatalogSnapshot, DatasetCatalog, PartitionCatalog, GenerationInfo
crates/geode-core/Cargo.toml            + tracing, tracing-subscriber
crates/geode-data/src/query/catalog.rs  NEW: build_catalog(conn, schema, as_of) -> CatalogSnapshot
crates/geode-data/src/service.rs        + DataEvent::{Polled, Catalog}; DataService::catalog; SchedulerEvent::Polled mapped
crates/geode-data/src/handle.rs         + Request::Catalog, DataHandle::catalog
crates/geode-data/src/ingest/scheduler.rs  Polled gains `next_in`
crates/geode-data/src/ingest/runner.rs  panic path logs at error with file + payload
crates/geode-data/src/**                eprintln! sites → tracing (none today; the crate is silent — verify)
crates/geode-shell/src/diagnostics.rs   NEW: Diagnostics entity, SourceState, DatasetState, summary(), pending drains, ActionTail
crates/geode-shell/src/log_persist.rs   NEW: persist_log_level_to_user_config
crates/geode-shell/src/shell/mod.rs     + diagnostics: Entity<Diagnostics>, diagnostics(), open_module(kind), action tail recording, data_status deleted
crates/geode-shell/src/shell/occupants.rs  pending_kind_for_new_tile consumed when creating occupants
crates/geode-shell/src/shell/hot_reload.rs [log] reload → LevelControl::set; config diagnostics → entity
crates/geode-shell/src/shell/status.rs  summary from Diagnostics replaces data_status_message
crates/geode-shell/src/shell/input.rs   diagnostics::open, perf::toggle_overlay reachable from the entity drain
crates/geode-shell/src/defaults.rs      `mod+shift+d` = diagnostics::open; `diagnostics` key context bindings
crates/geode-shell/src/module.rs        ModuleFactory::create gains `diagnostics: Entity<Diagnostics>`
crates/geode-shell/src/**               eprintln! sites → tracing
crates/geode-diagnostics/               NEW crate: lib.rs (init, DiagnosticsFactory), tile.rs (DiagnosticsTile), sections.rs (pure row builders), commands.rs (pure `:section`/`:level`/`:overlay` parser)
crates/geode-app/src/main.rs            subscriber + file layer + panic hook; roster gains the diagnostics factory; eprintln! → tracing
crates/geode-app/src/bridge.rs          Polled/Catalog/Health/Published/Diagnostics → Diagnostics entity; catalog request on visibility/publish
crates/geode-app/src/crash.rs           NEW: install_panic_hook(dir, ring, tail, registry names)
crates/geode-blotter/src/content.rs     create() signature; tile.rs eprintln → tracing
Cargo.toml                              workspace deps: tracing, tracing-subscriber, tracing-appender
scripts/mutation-check.sh               entries per task
docs/perf.md, CLAUDE.md, the spec       Task 8
```

## Rulings taken while planning (record, do not re-decide)

- **Open question 2 (where the frame histogram lives):** `ShellView`
  keeps `perf: FrameHistogram` and the overlay keeps reading it on the
  render path. `Diagnostics.frame_hist` is a *copy* refreshed on the
  existing reload-poll tick (`hot_reload::RELOAD_POLL_INTERVAL`, the
  500 ms loop in `ShellView::new`) only while `Diagnostics.watchers > 0`
  — a diagnostics tile increments `watchers` when visible. Bounded, no
  per-frame churn, one value of record.
- **`SourceState.since`/`last_poll`/`next_poll`** need a `Polled` event
  the shell never had: `SchedulerEvent::Polled` gains `next_in:
  Duration` (the spec's `poll_interval`), and the service maps it to
  `DataEvent::Polled { source, ready, at: SystemTime, next: SystemTime }`.
- **Where the log files go:** the spec says "the user data dir". This
  codebase has one user directory, `config_dirs().1`
  (`%APPDATA%/geode` or `$HOME/.config/geode`); logs go to
  `<that>/logs/geode.YYYY-MM-DD.log` and crash files to
  `<that>/crash-<timestamp>.log`. `None` (no home) means no file layer
  and no crash file, never a panic.
- **`Request::Catalog` runs on the service thread**, not a pool worker:
  it is a handful of catalog-sized queries (the `generations` table,
  `file_generations`, `duckdb_tables()`, `pragma_database_size()`,
  `duckdb_memory()`), none of which touch a data table's rows. It must
  stay that way — Task 3's doc comment says so.
- **The panic in ingest marks the source `Failed`, not `Degraded`.**
  The runner already maps a panic to `IngestEvent::Failed`, which the
  service reports as `Health::Failed { reason }`. A panic is a failed
  load, and the last good generation stays live — `Failed` is the
  honest label. Task 6 adds the `error` event with file and payload and
  records the deviation from §4.7's wording in the spec's as-built note.
- **The action tail stores FNV-1a hashes**, not ids: `ActionId` is a
  `String`, and cloning one allocates. `ActionRegistry` keeps a
  `hash → id` map filled at `register`, and the crash hook resolves the
  tail through it. Recording is a hash of `&str` into `[u64; 32]`.
- **`diagnostics::open` opens by kind through the shell**, not through
  the module: `ShellView::open_module(kind)` focuses an existing tile of
  that kind in the current workspace or splits the focused tile and
  sets `pending_kind_for_new_tile`, which `sync_occupants` consumes.
  Kept on the diagnostics module's own merits (a tile opened from a
  chord, not from the default kind); Phase 4c was redesigned on
  2026-09-08 (`docs/superpowers/specs/2026-09-08-geode-phase-4c-config-dialogs-design.md`)
  and ships no tile, so it is not the justification.
- **Config write paths stay separate in 4b.** 4c's first task introduces
  a `config_write` door and migrates every persist onto it. Task 4's
  `:level` persist therefore reuses `theme::write_atomic` and the
  `toml_edit` pattern of `persist_slot_to_user_config` as a seventh
  caller — it adds no new atomic-write implementation — and 4c migrates
  it with the rest. Nothing in 4b consolidates the existing six.
- **`Diagnostic` gains `path: Option<String>` here, not in 4c.** 4c
  attaches a reader's key path to a dialog field row and planned to add
  the field; Task 4 adds it (default `None`, `Diagnostic::error`/`warning`
  unchanged, a `with_path` builder) so 4c consumes it. Readers do not
  fill it in 4b.
- **`ShellEvent::ReloadRejected(Vec<Diagnostic>)`** is coming from 4c;
  4b's `apply_reload` feed into the entity does not depend on it.
- **`:overlay` toggles the shell's overlay through the entity:** the
  tile sets `Diagnostics.pending_overlay_toggle`; `ShellView`'s
  observer drains it and flips `perf_overlay`. Modules never reach
  `ShellView`.

---

### Task 1: The deferred minors from 4a's final review (M2, M4–M15)

The 4a final review recorded fifteen minors; M1 and M3 are done. Each
of the rest is small; together they are one task with one review. The
anchors below were verified on 2026-09-08 against `a053767`.

**Files:**
- Modify: `crates/geode-shell/src/frame.rs` (`end_scope_session` ~line 276; `save_scope` ~line 469–478)
- Modify: `crates/geode-shell/src/scopebar.rs` (`format!` at ~55, 57, 64, 71; the `today` input ~line 34–45)
- Modify: `crates/geode-shell/src/shell/picker.rs` (`tag: 0` at ~line 248)
- Modify: `crates/geode-blotter/src/tile.rs` (`view()` ~line 259)
- Modify: `crates/geode-shell/src/shell/mod.rs` (`FLIP_DEADLINE` timer ~line 1096; `visible_tile_keys`; `saved_scopes` ~line 223)
- Modify: `crates/geode-shell/src/shell/asof_view.rs` (`presets` ~line 71)
- Modify: `crates/geode-core/src/scope/expr.rs` (`impl Display for Expr` ~line 131)
- Modify: `crates/geode-shell/src/session.rs` (`FrameRecord` `dimensions` ~line 183–197)
- Modify: `docs/superpowers/specs/2026-09-06-geode-phase-4-frame-features-design.md` (§3.3, §3.10)
- Test: the tests beside each site

**Interfaces:**
- Consumes: everything above as it stands.
- Produces: no new interfaces; `Frame::save_scope` no longer bumps
  `versions.config`; `scopebar::build_model` takes `today: NaiveDate`
  explicitly; `shell::saved_scopes` is deleted in favour of
  `hot_reload::rebuild_saved_scopes`.

The items, each with its test (RED first — record the failing
assertion — then GREEN), in commit order:

- [ ] **M2 — escape after a text session leaves a no-op undo entry.**
  `Frame::end_scope_session` clears `scope_session`; if the session
  pushed the base scope (`Some(None)`) but the scope is now *equal* to
  that base (typed then deleted), pop the entry. Test
  `a_text_session_that_ends_where_it_began_leaves_no_undo_entry`: begin
  session, set text `a`, set text `""`, end session → `undo_scope()`
  returns `false`/no-op and `scope_undo.len()` unchanged. Implement by
  keeping the base in `Some(Some(base))` until end, comparing, and
  popping when equal:

  ```rust
  pub fn end_scope_session(&mut self) {
      if let Some(Some(base)) = self.scope_session.take()
          && self.scope_undo.last() == Some(&base)
          && self.scope == base
      {
          self.scope_undo.pop();
      }
  }
  ```
  (Read the current `begin_scope_session`/`push_undo` first: the state
  machine's comments say `Some(None)` means "already pushed"; change
  that to keep the base and update the comment — the push still
  happens once.)

- [ ] **M4 — three per-render `format!`s the bar model should own.**
  `scopebar.rs` lines ~55–71 format chip labels inside the render path
  every paint. Move them into `build_model` so `BarModel` carries the
  finished `String`s; the renderer only clones `SharedString`s. Test:
  `build_model` output contains the formatted label
  (`"book ∈ BK000, BK001"`) and the render function takes `&BarModel`
  only (compile-time: it has no `format!`).

- [ ] **M5 — picker `tag` resets per open.** `picker.rs` ~248
  `tag: 0` on every `PickerState::new`: a stale outcome from a previous
  open with the same column and tag 0 would be accepted. Keep a
  `next_picker_tag: u64` on `ShellView` and pass `tag: self.next_picker_tag`
  (incremented per open). Test in `shell/tests/picker.rs`: open, close,
  open again → the second `DistinctRequested` carries a larger tag.

- [ ] **M6 — `ViewSpec` file order changes the default view.**
  `BlotterTile::view()` finds by `view_name`; the *default* view for a
  new tile is `views[0]`. With `toml` `preserve_order` on, that is file
  order. Make the default explicit: `ViewSpec::from_doc` returns views
  sorted by name unless the doc sets `default = "<name>"` at the top
  level; `BlotterTile::new` picks the one flagged `default` else the
  first by name. Test in `geode-core::view`: two views declared `b`
  then `a` → `from_doc` yields `a, b`; with `default = "b"` the spec
  named `b` carries `is_default = true`.

- [ ] **M7 — one detached timer per mutation.** `shell/mod.rs` ~1096
  spawns a `FLIP_DEADLINE` timer on every scope/grouping/as-of change;
  a burst of keystrokes spawns one per keystroke. Replace with one
  sweep on the existing reload-poll tick: in the 500 ms loop, call
  `frame.update(|f, cx| if f.sweep(Instant::now()) { cx.notify() })`.
  The deadline is 250 ms, so a barrier is released at most 500 ms
  after opening instead of exactly 250 ms; the spec's "250 ms deadline"
  becomes "released on the next tick after 250 ms" — write that in the
  §3.10 as-built note (M14). Test in `shell/tests/`: open a barrier,
  advance the test executor past one tick, assert released; no timer
  is spawned per mutation (assert via `cx.background_executor()`'s
  pending-task count staying flat across ten mutations, if the test
  executor exposes it; else assert the code path by a `#[cfg(test)]`
  counter on `ShellView` — `barrier_timers_spawned`, expected 0).

- [ ] **M8 — placeholder occupants in the barrier set.**
  `visible_tile_keys` includes tiles whose occupant is a
  `PlaceholderFactory` product; they never arrive, so every barrier
  waits for the deadline. Skip occupants whose `kind == "placeholder"`.
  Test: a shell with one blotter and one placeholder tile opens a
  barrier of size one.

- [ ] **M9 — presets rebuilt per render.** `asof_view::presets(frame)`
  is called from the render path; cache on `Frame::versions().publishes`
  (add the field if `FrameVersions` lacks one — `note_published` bumps
  it) in the modal's state. Test: two renders without a publish call
  `presets` once (count via a `#[cfg(test)]` counter).

- [ ] **M10 — `Expr` Display has no `'` escape.** `expr.rs` ~131:
  a string literal containing `'` renders unquoted-broken. Escape as
  `''` (the SQL convention the parser must accept back). Test:
  `parse_expr(&format!("{}", expr))` round-trips `book = 'O''Neil'`.

- [ ] **M11 — `save_scope` bumps `config`.** `frame.rs` ~477
  `self.versions.config += 1` after saving a scope makes every tile
  requery. Bump a new `versions.saved_scopes` instead; `follows_changed`
  in the blotter ignores it; the palette's `scope::<name>` list rebuilds
  on it. Test: `save_scope` leaves `versions().config` unchanged and
  bumps `saved_scopes`.

- [ ] **M12 — bar cache ignores midnight.** `scopebar::build_model`
  formats an as-of as `HH:MM` when its date is today; the cache key is
  `FrameVersions` only, so after midnight the label is stale until the
  next mutation. Include `today: NaiveDate` in the cache key
  (`bar_model: Option<(FrameVersions, NaiveDate, BarModel)>`). Test:
  same versions, different `today` → rebuilt.

- [ ] **M13 — `FrameRecord` always writes `dimensions`.** `session.rs`
  ~197 inserts an empty `dimensions` table even when there are none.
  Omit it when empty; `from_toml` already treats absence as empty.
  Test: a frame with no selections serialises without a `dimensions`
  key; round-trip unchanged.

- [ ] **M14 — spec §3.10/§3.3 as-built notes.** §3.10: the deadline is
  swept on the reload tick (M7); §3.3: the values list is a
  `uniform_list`. Two short paragraphs.

- [ ] **M15 — `shell::saved_scopes` duplicates `rebuild_saved_scopes`.**
  Delete `shell::saved_scopes` (`shell/mod.rs` ~223) and call
  `hot_reload::rebuild_saved_scopes` from its one caller. Test: the
  existing saved-scopes tests stay green.

- [ ] **Harness entries** (one per behaviour, verified by hand): M2
  (the pop removed → the new test), M5 (`next_picker_tag` not
  incremented), M7 (the sweep removed from the tick → the barrier
  test), M8 (the placeholder skip removed), M10 (the escape removed),
  M11 (`config` bumped again), M12 (`today` dropped from the key),
  M13 (always-insert restored). Update `CLAUDE.md`'s count.

- [ ] **Commit** per item (`fix(shell): M<n> — <one line>`), then the
  harness commit.

---

### Task 2: `tracing` foundation — the ring, the levels, the subscriber, the migration

**Files:**
- Create: `crates/geode-core/src/log/mod.rs`
- Modify: `crates/geode-core/src/lib.rs` (`pub mod log;`), `crates/geode-core/Cargo.toml`
- Modify: `Cargo.toml` (workspace deps), `crates/geode-app/Cargo.toml`, `crates/geode-shell/Cargo.toml`, `crates/geode-data/Cargo.toml`, `crates/geode-blotter/Cargo.toml`
- Modify: `crates/geode-app/src/main.rs` (subscriber first; `[log]` read; file layer)
- Modify: `crates/geode-shell/src/shell/mod.rs` (`ShellServices.log`)
- Modify: every `eprintln!` site listed below
- Test: `crates/geode-core/src/log/mod.rs` tests; `crates/geode-app/src/main.rs` tests

**Interfaces:**
- Consumes: `Config::doc("app")`, `config_dirs()`.
- Produces:
  ```rust
  // geode_core::log
  pub use tracing::Level;
  pub struct Record { pub at: SystemTime, pub level: Level, pub target: &'static str, pub message: String, pub seq: u64 }
  pub struct Ring { /* Mutex<RingInner> */ }
  impl Ring {
      pub fn new(capacity: usize) -> Ring;          // spec: 4_096 in main
      pub fn push(&self, r: Record);
      pub fn drain_since(&self, since: u64, out: &mut Vec<Record>);   // clears `out`; records with seq > since, oldest first
      pub fn latest_seq(&self) -> u64;
      pub fn capacity(&self) -> usize;
  }
  pub struct RingLayer { ring: Arc<Ring> }         // tracing_subscriber::Layer<S>
  impl RingLayer { pub fn new(ring: Arc<Ring>) -> RingLayer; }
  #[derive(Clone, Debug, PartialEq, Eq)]
  pub struct LogLevels { pub default: Level, pub targets: Vec<(String, Level)> }   // target suffixes: "ingest" → geode::ingest
  impl LogLevels {
      pub fn from_doc(config: &Config) -> (LogLevels, Vec<Diagnostic>);   // reads [log]; unknown level → warning, key kept at default
      pub fn to_targets(&self) -> tracing_subscriber::filter::Targets;
      pub fn with(&self, target: &str, level: Level) -> LogLevels;          // for :level
  }
  pub trait LevelControl: Send + Sync { fn set(&self, levels: &LogLevels) -> Result<(), String>; }
  pub const TARGETS: [&str; 6] = ["geode::ingest", "geode::query", "geode::config", "geode::session", "geode::shell", "geode::theme"];
  // geode_shell::shell::ShellServices
  pub log: Option<LogServices>,
  pub struct LogServices { pub ring: Arc<Ring>, pub control: Arc<dyn LevelControl>, pub levels: LogLevels }
  ```

- [ ] **Step 1: Dependencies.** In the workspace `Cargo.toml`
  `[workspace.dependencies]` add:

  ```toml
  tracing = "0.1.44"
  tracing-subscriber = { version = "0.3.23", default-features = false, features = ["std", "fmt", "registry"] }
  tracing-appender = "0.2"
  ```

  `tracing`/`tracing-subscriber` are already in `Cargo.lock` at these
  versions through gpui; `tracing-appender` is new — run `cargo update
  -p tracing-appender --precise <resolved>` once and commit the lock.
  `geode-core` takes `tracing.workspace = true` and
  `tracing-subscriber.workspace = true`; `geode-shell`, `geode-data`,
  `geode-blotter` take `tracing.workspace = true` only; `geode-app`
  takes all three. Run `cargo check --workspace`.

- [ ] **Step 2: Write the failing ring tests** in
  `crates/geode-core/src/log/mod.rs`:

  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;
      fn rec(seq: u64, msg: &str) -> Record {
          Record { at: std::time::SystemTime::UNIX_EPOCH, level: Level::INFO, target: "geode::shell", message: msg.into(), seq }
      }
      #[test]
      fn drain_since_returns_only_newer_records_oldest_first() {
          let ring = Ring::new(4);
          for i in 1..=3 { ring.push(rec(i, &format!("m{i}"))); }
          let mut out = vec![rec(0, "stale")];
          ring.drain_since(1, &mut out);
          assert_eq!(out.iter().map(|r| r.seq).collect::<Vec<_>>(), vec![2, 3], "cleared first, then seq > since");
          assert_eq!(ring.latest_seq(), 3);
      }
      #[test]
      fn wrapping_overwrites_the_oldest_and_keeps_order() {
          let ring = Ring::new(3);
          for i in 1..=5 { ring.push(rec(i, "m")); }
          let mut out = Vec::new();
          ring.drain_since(0, &mut out);
          assert_eq!(out.iter().map(|r| r.seq).collect::<Vec<_>>(), vec![3, 4, 5]);
      }
      #[test]
      fn a_hit_allocates_nothing_in_the_reader() {
          // The reader's buffer is reused: after one drain that returned
          // n records, a second drain returning n records must not grow
          // the Vec's capacity.
          let ring = Ring::new(8);
          for i in 1..=4 { ring.push(rec(i, "m")); }
          let mut out = Vec::new();
          ring.drain_since(0, &mut out);
          let cap = out.capacity();
          for i in 5..=8 { ring.push(rec(i, "m")); }
          ring.drain_since(4, &mut out);
          assert_eq!(out.len(), 4);
          assert_eq!(out.capacity(), cap);
      }
      #[test]
      fn two_writers_never_lose_a_sequence_number() {
          let ring = std::sync::Arc::new(Ring::new(1024));
          let a = { let r = ring.clone(); std::thread::spawn(move || for _ in 0..500 { r.push(rec(0, "a")); }) };
          let b = { let r = ring.clone(); std::thread::spawn(move || for _ in 0..500 { r.push(rec(0, "b")); }) };
          a.join().unwrap(); b.join().unwrap();
          let mut out = Vec::new();
          ring.drain_since(0, &mut out);
          assert_eq!(out.len(), 1000);
          assert!(out.windows(2).all(|w| w[1].seq == w[0].seq + 1), "seq is assigned by the ring, contiguous");
      }
      #[test]
      fn levels_read_the_log_table_and_report_a_bad_level() {
          let cfg = crate::config::test_support::config_from("app", "config_version = 1\n[log]\ndefault = \"info\"\ningest = \"debug\"\nquery = \"loud\"\n");
          let (levels, diags) = LogLevels::from_doc(&cfg);
          assert_eq!(levels.default, Level::INFO);
          assert_eq!(levels.targets, vec![("ingest".to_string(), Level::DEBUG)]);
          assert_eq!(diags.len(), 1);
          assert!(diags[0].message.contains("loud"));
      }
      #[test]
      fn to_targets_maps_suffixes_onto_geode_targets() {
          let levels = LogLevels { default: Level::WARN, targets: vec![("ingest".into(), Level::TRACE)] };
          let t = levels.to_targets();
          assert!(t.would_enable("geode::ingest", &Level::TRACE));
          assert!(!t.would_enable("geode::query", &Level::INFO));
          assert!(t.would_enable("geode::query", &Level::WARN));
      }
  }
  ```

  Note the `seq` rule the two-writer test pins: **the ring assigns
  `seq`**, ignoring the caller's, so the layer never needs a counter.
  If `config::test_support::config_from` does not exist, add it under
  `#[cfg(any(test, feature = "test-support"))]`: build a `Config` from
  one builtin `LayerDoc` (the `merge_docs`/`LayerDoc::builtin` pattern
  the benches use).

- [ ] **Step 3: Run** `cargo test -p geode-core log::` — expected:
  compile failure (module missing).

- [ ] **Step 4: Implement `log/mod.rs`:**

  ```rust
  //! The in-process log (spec §4.1–4.3): a fixed ring every subscriber
  //! layer feeds, read by the diagnostics tile; `[log]` levels; the
  //! control the shell uses to change them at runtime.
  use crate::config::{Config, Diagnostic, Layer};
  use std::sync::{Arc, Mutex};
  use std::time::SystemTime;
  pub use tracing::Level;
  use tracing_subscriber::filter::Targets;

  pub const TARGETS: [&str; 6] = ["geode::ingest", "geode::query", "geode::config", "geode::session", "geode::shell", "geode::theme"];

  #[derive(Clone, Debug, PartialEq, Eq)]
  pub struct Record { pub at: SystemTime, pub level: Level, pub target: &'static str, pub message: String, pub seq: u64 }

  struct RingInner { records: Box<[Option<Record>]>, head: usize, seq: u64 }

  pub struct Ring { inner: Mutex<RingInner> }

  impl Ring {
      pub fn new(capacity: usize) -> Ring {
          let capacity = capacity.max(1);
          Ring { inner: Mutex::new(RingInner { records: (0..capacity).map(|_| None).collect(), head: 0, seq: 0 }) }
      }
      pub fn capacity(&self) -> usize { self.inner.lock().unwrap_or_else(|e| e.into_inner()).records.len() }
      /// Overwrites the oldest slot once full; never blocks a writer for
      /// longer than one copy. `seq` is assigned here, contiguous, so two
      /// threads racing on `push` cannot produce a gap or a duplicate.
      pub fn push(&self, mut r: Record) {
          let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
          g.seq += 1;
          r.seq = g.seq;
          let cap = g.records.len();
          let head = g.head;
          g.records[head] = Some(r);
          g.head = (head + 1) % cap;
      }
      pub fn latest_seq(&self) -> u64 { self.inner.lock().unwrap_or_else(|e| e.into_inner()).seq }
      /// Records with `seq > since`, oldest first, into `out` (cleared
      /// first). The reader owns the buffer: a tile following the tail
      /// reuses one `Vec` for its life, so a hit allocates nothing beyond
      /// the record clones themselves.
      pub fn drain_since(&self, since: u64, out: &mut Vec<Record>) {
          out.clear();
          let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
          let cap = g.records.len();
          // Oldest slot is `head` once wrapped, else 0.
          for i in 0..cap {
              let idx = (g.head + i) % cap;
              if let Some(r) = &g.records[idx] && r.seq > since {
                  out.push(r.clone());
              }
          }
      }
  }

  /// The subscriber layer that fills the ring. Formats the message on
  /// the emitting thread, outside the ring's lock.
  pub struct RingLayer { ring: Arc<Ring> }
  impl RingLayer { pub fn new(ring: Arc<Ring>) -> RingLayer { RingLayer { ring } } }

  struct MessageVisitor(String);
  impl tracing::field::Visit for MessageVisitor {
      fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
          use std::fmt::Write;
          if field.name() == "message" {
              let _ = write!(self.0, "{value:?}");
          } else {
              if !self.0.is_empty() { self.0.push(' '); }
              let _ = write!(self.0, "{}={value:?}", field.name());
          }
      }
      fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
          use std::fmt::Write;
          if field.name() == "message" { self.0.push_str(value); } else {
              if !self.0.is_empty() { self.0.push(' '); }
              let _ = write!(self.0, "{}={value}", field.name());
          }
      }
  }

  impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for RingLayer {
      fn on_event(&self, event: &tracing::Event<'_>, _ctx: tracing_subscriber::layer::Context<'_, S>) {
          let mut v = MessageVisitor(String::new());
          event.record(&mut v);
          let meta = event.metadata();
          self.ring.push(Record { at: SystemTime::now(), level: *meta.level(), target: meta.target(), message: v.0, seq: 0 });
      }
  }

  #[derive(Clone, Debug, PartialEq, Eq)]
  pub struct LogLevels { pub default: Level, pub targets: Vec<(String, Level)> }

  fn parse_level(s: &str) -> Option<Level> {
      match s.to_ascii_lowercase().as_str() {
          "error" => Some(Level::ERROR), "warn" | "warning" => Some(Level::WARN), "info" => Some(Level::INFO),
          "debug" => Some(Level::DEBUG), "trace" => Some(Level::TRACE), _ => None,
      }
  }

  impl Default for LogLevels { fn default() -> Self { LogLevels { default: Level::INFO, targets: Vec::new() } } }

  impl LogLevels {
      /// `[log]` in the `app` doc: `default = "info"`, then one key per
      /// target suffix (`ingest = "debug"`). An unknown level is a
      /// warning and the key keeps the default; an unknown key is a
      /// warning too (a typo must not silence a target).
      pub fn from_doc(config: &Config) -> (LogLevels, Vec<Diagnostic>) {
          let mut levels = LogLevels::default();
          let mut diags = Vec::new();
          let Some(table) = config.get("app", "log").and_then(|v| v.as_table()) else { return (levels, diags); };
          let file = config.doc("app").and_then(|d| d.provenance_of("log")).map(|p| p.file.clone());
          let warn = |msg: String| Diagnostic::warning(Layer::User, file.clone().unwrap_or_default(), msg);
          for (key, value) in table {
              let Some(s) = value.as_str() else { diags.push(warn(format!("[log] {key}: expected a level string"))); continue; };
              let Some(level) = parse_level(s) else { diags.push(warn(format!("[log] {key} = {s:?}: not a level (error, warn, info, debug, trace)"))); continue; };
              if key == "default" { levels.default = level; continue; }
              if !TARGETS.iter().any(|t| t.strip_prefix("geode::") == Some(key.as_str())) {
                  diags.push(warn(format!("[log] {key}: not a known target ({})", TARGETS.iter().map(|t| &t[7..]).collect::<Vec<_>>().join(", "))));
                  continue;
              }
              levels.targets.retain(|(k, _)| k != key);
              levels.targets.push((key.clone(), level));
          }
          (levels, diags)
      }
      pub fn to_targets(&self) -> Targets {
          let mut t = Targets::new().with_default(self.default);
          for (suffix, level) in &self.targets { t = t.with_target(format!("geode::{suffix}"), *level); }
          t
      }
      pub fn with(&self, target: &str, level: Level) -> LogLevels {
          let mut out = self.clone();
          out.targets.retain(|(k, _)| k != target);
          out.targets.push((target.to_string(), level));
          out
      }
  }

  pub trait LevelControl: Send + Sync { fn set(&self, levels: &LogLevels) -> Result<(), String>; }
  ```

  `provenance_of` — use whatever `MergedDoc` exposes for a key's
  provenance (read `config/mod.rs`; if it has none, pass `None` for the
  file and use `Layer::User`). The layer choice for a `[log]`
  diagnostic follows `Config::explain("app", "log")` when available.

- [ ] **Step 5: Run** `cargo test -p geode-core log::` — expected: all
  six pass.

- [ ] **Step 6: The subscriber in `main.rs`**, before anything else
  (before `Application::new`, before config load — the config load
  itself should log). Order: build the ring; install the subscriber
  with `Targets` from `LogLevels::default()` behind a `reload::Layer`;
  load config; apply `LogLevels::from_doc(&config)` through the reload
  handle (so `[log]` takes effect for everything after the load); add
  the file layer if the user dir exists. Sketch:

  ```rust
  use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, reload, fmt};
  let ring = Arc::new(geode_core::log::Ring::new(4096));
  let (filter, reload_handle) = reload::Layer::new(geode_core::log::LogLevels::default().to_targets());
  let (user_dir, _) = ...; // config_dirs()
  let file_layer = user_dir.as_ref().map(|dir| {
      let logs = dir.join("logs");
      let _ = std::fs::create_dir_all(&logs);
      crash::trim_log_files(&logs, 7); // the seven-file cap, applied at startup
      let appender = tracing_appender::rolling::daily(&logs, "geode.log");
      fmt::layer().with_writer(appender).with_ansi(false)
  });
  tracing_subscriber::registry()
      .with(filter)
      .with(fmt::layer().with_writer(std::io::stderr))
      .with(geode_core::log::RingLayer::new(ring.clone()))
      .with(file_layer)
      .init();
  struct ReloadControl(reload::Handle<Targets, Registry>);
  impl LevelControl for ReloadControl { fn set(&self, l: &LogLevels) -> Result<(), String> { self.0.reload(l.to_targets()).map_err(|e| e.to_string()) } }
  ```

  `tracing-appender` names files `geode.log.YYYY-MM-DD`; the spec says
  `geode.YYYY-MM-DD.log`. Use `RollingFileAppender::builder()
  .rotation(Rotation::DAILY).filename_prefix("geode").filename_suffix("log")`
  which yields `geode.YYYY-MM-DD.log`. `trim_log_files(dir, keep)`
  (in `crash.rs`, created here as a pure function of a directory
  listing) deletes the oldest `geode.*.log` beyond `keep`; test it on a
  tempdir with nine files. `ShellServices.log = Some(LogServices {
  ring, control: Arc::new(ReloadControl(handle)), levels })`.

- [ ] **Step 7: The migration.** Replace every site with an event on
  the named target at the named level (the list is exhaustive as of
  `a053767`; grep again before you start):

  | Site | Target | Level |
  |---|---|---|
  | `geode-app/src/main.rs:37` (fatal startup message) | `geode::config` | error |
  | `main.rs:51` (demo emit failed) | `geode::ingest` | error |
  | `main.rs:82`, `:315` (theme warning) | `geode::theme` | warn |
  | `main.rs:96` (session warning) | `geode::session` | warn |
  | `main.rs:343` (startup diagnostics by severity) | `geode::config` | error/warn by severity |
  | `bridge.rs:148`, `:321` (data diagnostics) | `geode::query` | warn |
  | `bridge.rs:293` (published) | `geode::ingest` | info |
  | `bridge.rs:314` (health) | `geode::ingest` | warn (`Failed` → error) |
  | `geode-shell/src/fonts.rs:100` | `geode::theme` | warn |
  | `shell/session_io.rs:73`, `:103`; `shell/mod.rs:851` | `geode::session` | warn |
  | `shell/keybindings_view.rs:621`, `:641`, `:649` | `geode::config` | warn |
  | `shell/profiling_hook.rs:55`, `:81` | `geode::shell` | info (a dump the user asked for) |
  | `shell/hot_reload.rs:56`, `:81`, `:154` | `geode::config` | warn |
  | `shell/mod.rs:1114`, `:1129` | `geode::config` | warn |
  | `shell/input.rs:335`, `:354`, `:373` | `geode::theme` / `geode::config` | warn |
  | `geode-blotter/src/tile.rs:185` | `geode::shell` | warn |

  Use `tracing::warn!(target: "geode::config", "{d}")` — the message
  formatted as today, minus the `[source]` prefix (the target carries
  it). Anything on the UI thread stays at `warn` or above. `geode-data`
  has no `eprintln!` today; add `info!(target: "geode::ingest", …)` at
  publish (`service.rs`'s `IngestEvent::Published` arm) and at the
  scheduler's `Health` arm, and `debug!` at `Polled`. Test: `grep -rn
  "eprintln!" crates/*/src | grep -v "/tests/\|#\[cfg(test)\]"` is empty
  (write it as a `#[test]` in `geode-app` that walks the workspace's
  `src` directories and fails on a match outside test modules, so it
  stays true).

- [ ] **Step 8: Run** the full CI set. Commit
  `feat(log): tracing foundation — ring, levels, subscriber, migration`.

- [ ] **Step 9: Harness entries** (verified by hand): the ring's wrap
  (`(head + 1) % cap` → `head + 1` guarded… choose a mutation that
  keeps compiling: `g.head = (head + 1) % cap` → `g.head = head` —
  every push overwrites slot 0; caught by
  `wrapping_overwrites_the_oldest_and_keeps_order`); `drain_since`'s
  `r.seq > since` → `>=` (caught by
  `drain_since_returns_only_newer_records_oldest_first`); `seq` assigned
  by the caller (`r.seq = g.seq` removed — caught by the two-writer
  test); `from_doc` accepting an unknown target (the `TARGETS` check
  removed — caught by a new test `an_unknown_target_key_is_a_warning`).
  Commit.

---

### Task 3: `Request::Catalog` — what the database holds, verified by execution

**Files:**
- Modify: `crates/geode-core/src/query.rs` (the `Catalog` types)
- Create: `crates/geode-data/src/query/catalog.rs`
- Modify: `crates/geode-data/src/query/mod.rs` (`pub mod catalog; pub use catalog::build_catalog;`)
- Modify: `crates/geode-data/src/service.rs` (`DataEvent::Catalog`, `DataService::catalog`, the handle's request loop)
- Modify: `crates/geode-data/src/handle.rs` (`Request::Catalog`, `DataHandle::catalog`)
- Test: `catalog.rs` tests against a real store; `handle.rs`/`service.rs` tests

**Interfaces:**
- Consumes: `resolve_generations(conn, dataset, at)`; the `generations` and `file_generations` tables; `SchemaSpec`; `table_name`.
- Produces:
  ```rust
  // geode_core::query
  pub struct CatalogParams { pub key: QueryKey, pub tag: u64, pub as_of: AsOf }
  pub struct CatalogOutcome { pub key: QueryKey, pub tag: u64, pub snapshot: Result<CatalogSnapshot, String> }
  #[derive(Clone, Debug, PartialEq, Eq, Default)]
  pub struct CatalogSnapshot {
      pub datasets: Vec<DatasetCatalog>,
      pub database_bytes: u64,      // block_size * total_blocks from pragma_database_size()
      pub used_blocks: u64,         // used_blocks from the same
      pub memory_bytes: u64,        // sum(memory_usage_bytes) from duckdb_memory()
      pub threads: u64,             // current_setting('threads')
  }
  pub struct DatasetCatalog { pub name: String, pub partitions: Vec<PartitionCatalog>, pub live_rows: u64, pub archive_rows: u64 }
  pub struct PartitionCatalog { pub batch: String, pub book: Option<String>, pub generations: Vec<GenerationInfo>, pub resolved_gen: Option<i64> }
  pub struct GenerationInfo { pub gen_id: i64, pub source_time: DateTime<Utc>, pub loaded_at: Option<DateTime<Utc>>, pub file_rows: Option<u64>, pub live: bool }
  // geode_data
  pub fn build_catalog(conn: &Connection, schema: &SchemaSpec, as_of: &AsOf) -> Result<CatalogSnapshot, StoreError>;
  impl DataService { pub fn catalog(&self, params: &CatalogParams) -> CatalogOutcome; }
  impl DataHandle { pub fn catalog(&self, params: CatalogParams) -> bool; }
  pub enum Request { …, Catalog(CatalogParams) }
  pub enum DataEvent { …, Catalog(CatalogOutcome) }
  ```
  `geode-core` gains `chrono` if it lacks it (check `Cargo.toml`).

**The DuckDB functions, as verified on 2026-09-08 (DuckDB 1.10505):**

| Function | Columns (type) | Note |
|---|---|---|
| `pragma_database_size()` | `database_name VARCHAR, database_size VARCHAR, block_size BIGINT, total_blocks BIGINT, used_blocks BIGINT, free_blocks BIGINT, wal_size VARCHAR, memory_usage VARCHAR, memory_limit VARCHAR` | the sizes are **formatted strings** (`"512.0 KiB"`); bytes = `block_size * total_blocks` |
| `duckdb_memory()` | `tag VARCHAR, memory_usage_bytes BIGINT, temporary_storage_bytes BIGINT` | sum `memory_usage_bytes` |
| `duckdb_tables()` | `… table_name VARCHAR, estimated_size BIGINT, column_count BIGINT, index_count BIGINT …` | `estimated_size` is a **row** estimate |
| `current_setting('threads')` | VARCHAR | parse |
| `pragma_storage_info('t')` | `row_group_id, column_name, column_id, column_path, segment_id, segment_type, start, count, compression, stats, has_updates, persistent, block_id, block_offset` | per-segment; `count(distinct block_id) * block_size` approximates a table's bytes but shares blocks across tables — **not used**; per-table bytes are out of scope, rows are reported instead |

- [ ] **Step 1: Write the failing test** in `catalog.rs` using the
  two-generation fixture pattern from `distinct.rs`
  (`two_dataset_fixture_with_history`) or `load.rs`'s
  `tests_support::fixture` ingested twice:

  ```rust
  #[test]
  fn the_catalog_lists_every_partitions_generations_with_the_live_one_marked() {
      let f = fixture_with_two_generations();            // BK000: gen 1 archived, gen 2 live; BK001: gen 3 live only
      let snap = build_catalog(f.store.writer(), &f.schema, &AsOf::Live).unwrap();
      let ds = snap.datasets.iter().find(|d| d.name == "risk_snapshot").unwrap();
      let bk000 = ds.partitions.iter().find(|p| p.book.as_deref() == Some("BK000")).unwrap();
      assert_eq!(bk000.generations.iter().map(|g| (g.gen_id, g.live)).collect::<Vec<_>>(), vec![(1, false), (2, true)]);
      assert_eq!(bk000.resolved_gen, None, "live: nothing resolved");
      assert!(bk000.generations[1].loaded_at.is_some() && bk000.generations[1].file_rows.is_some());
      assert_eq!(ds.live_rows, f.live_rows_expected);       // from `select count(*)` in the test, not from estimated_size
      assert!(snap.database_bytes > 0 && snap.memory_bytes > 0 && snap.threads > 0);
  }
  #[test]
  fn under_an_as_of_the_resolved_generation_is_named_per_partition() {
      let f = fixture_with_two_generations();
      let snap = build_catalog(f.store.writer(), &f.schema, &AsOf::At(f.between)).unwrap();
      let ds = &snap.datasets[0];
      let bk000 = ds.partitions.iter().find(|p| p.book.as_deref() == Some("BK000")).unwrap();
      assert_eq!(bk000.resolved_gen, Some(1));
  }
  #[test]
  fn the_row_counts_agree_with_duckdb_by_execution() {
      // estimated_size is an estimate; on a freshly checkpointed table it equals count(*).
      let f = fixture_with_two_generations();
      f.store.writer().execute_batch("checkpoint;").unwrap();
      let snap = build_catalog(f.store.writer(), &f.schema, &AsOf::Live).unwrap();
      let live: i64 = f.store.writer().query_row("select count(*) from risk_snapshot_position_live", [], |r| r.get(0)).unwrap();
      assert_eq!(snap.datasets[0].live_rows, live as u64);
  }
  ```
  `live_rows` sums `estimated_size` over the dataset's live tables at
  every grain, `archive_rows` over the archive tables — both are what
  the tile shows, labelled "rows (est.)".

- [ ] **Step 2: Run** — expected: compile failure.

- [ ] **Step 3: Implement `build_catalog`:**
  1. `select batch, book, gen_id, source_time from generations where dataset = ? order by batch, book, source_time, gen_id`;
     group into partitions; `live` = the last row per partition in that
     order (newest by `(source_time, gen_id)` — the same tie-break
     `resolve_generations` uses).
  2. `select gen_id, loaded_at, row_count from file_generations where dataset = ?`
     into a `HashMap<i64, (DateTime, u64)>`; fill `loaded_at`/`file_rows`.
  3. When `as_of` is `At(t)`: `resolve_generations(conn, dataset, t)`
     → `resolved_gen` per `(batch, book)`.
  4. `select table_name, estimated_size from duckdb_tables()`; sum per
     dataset by matching `table_name(dataset, grain, kind)` for every
     grain in `ds.grains()`.
  5. `select block_size * total_blocks, used_blocks from pragma_database_size()`;
     `select coalesce(sum(memory_usage_bytes), 0) from duckdb_memory()`;
     `select current_setting('threads')` parsed as `u64`.
  Doc comment: runs on the service thread; every query above is
  catalog-sized; a data-table scan here would stall the request loop
  and is forbidden.

- [ ] **Step 4: Run** the tests — expected: pass.

- [ ] **Step 5: Wire the request.** `Request::Catalog(CatalogParams)`
  in `handle.rs`; `DataHandle::catalog` mirrors `distinct` (try-send,
  `false` when the queue is full). In the service's request loop the
  arm calls `build_catalog(&self.conn, &self.config.schema, &params.as_of)`
  and emits `DataEvent::Catalog(CatalogOutcome { key, tag, snapshot:
  result.map_err(|e| e.to_string()) })` through the sink. Test in
  `service.rs`: `open_channel`, `catalog(params)`, receive
  `DataEvent::Catalog` with the tag echoed and `datasets.len() == 1`.

- [ ] **Step 6: `Polled` reaches the shell.** `SchedulerEvent::Polled`
  gains `next_in: Duration` (set to `spec.poll_interval` at the emit
  site); the service maps it to
  `DataEvent::Polled { source, ready, at: SystemTime::now(), next: SystemTime::now() + next_in }`.
  Test in `scheduler.rs`: the existing unchanged-directory test also
  asserts `next_in == poll`.

- [ ] **Step 7: Run** the CI set; commit
  `feat(data): Request::Catalog and the Polled event`.

- [ ] **Step 8: Harness entries** (verified by hand): `live` computed
  as the *first* row per partition instead of the last (caught by the
  first test); `resolved_gen` ignored under `At` (`None` always —
  caught by the second); `live_rows` summing archive tables too
  (caught by the third); `Polled.next` = `at` (caught by the scheduler
  test). Commit.

---

### Task 4: The `Diagnostics` entity, its feed, and the status bar

**Files:**
- Create: `crates/geode-shell/src/diagnostics.rs`
- Modify: `crates/geode-shell/src/lib.rs` (`pub mod diagnostics;`)
- Modify: `crates/geode-shell/src/shell/mod.rs` (`diagnostics: Entity<Diagnostics>`, `pub fn diagnostics()`, the reload-tick copy of the histogram, the observer drains; delete `data_status`/`set_data_status`)
- Modify: `crates/geode-shell/src/shell/status.rs` (`data_status_message` → `diagnostics_summary: Option<&str>`)
- Modify: `crates/geode-shell/src/shell/render.rs` (call site)
- Modify: `crates/geode-shell/src/shell/hot_reload.rs` (config diagnostics into the entity on load and every reload; `[log]` change → `control.set`)
- Modify: `crates/geode-shell/src/module.rs` (`create` gains `diagnostics: Entity<Diagnostics>`), `crates/geode-blotter/src/content.rs` (accept and ignore it), `crates/geode-shell/src/module/placeholder.rs`
- Modify: `crates/geode-core/src/config/mod.rs` (`Diagnostic.path: Option<String>`, `with_path`; every literal construction site in the workspace gains `path: None` — grep `Diagnostic {`)
- Create: `crates/geode-shell/src/log_persist.rs` (`persist_log_level_to_user_config`, reusing `theme::write_atomic`)
- Modify: `crates/geode-app/src/bridge.rs` (every `DataEvent` lands in the entity; `Catalog` request on visibility and after `Published` while visible)
- Test: `diagnostics.rs` (pure); `shell/tests/diagnostics.rs` (new file: status bar summary, reload feed, overlay drain); `bridge.rs` tests

**Interfaces:**
- Consumes: `DataEvent::{Published, Health, Diagnostics, Polled, Catalog}`; `LogServices`; `FrameHistogram`; `RequeryStats` (read from `Frame`).
- Produces:
  ```rust
  // geode_shell::diagnostics
  pub struct SourceSummary { pub paths: Vec<String>, pub priority: String, pub readiness: String }   // from SourceSpec, filled once by the bridge at start
  pub struct SourceState { pub spec: Option<SourceSummary>, pub health: Health, pub detail: String, pub since: SystemTime, pub last_poll: Option<SystemTime>, pub next_poll: Option<SystemTime>, pub last_ready: usize, pub history: VecDeque<(SystemTime, Health)> /* cap 16 */ }
  pub struct DatasetState { pub catalog: Option<DatasetCatalog> }
  pub struct Diagnostics {
      pub sources: BTreeMap<String, SourceState>,
      pub datasets: BTreeMap<String, DatasetState>,
      pub config: VecDeque<(SystemTime, Diagnostic)>,   // latest first, cap 256
      pub dropped_events: u64,
      pub restart_required: Option<String>,
      pub frame_hist: FrameHistogram,
      pub catalog: Option<CatalogSnapshot>,             // whole snapshot, database_bytes etc.
      pub levels: LogLevels,
      watchers: u32,
      version: u64,
      pending_level: Option<(String, Level)>,
      pending_overlay_toggle: bool,
      pending_catalog_request: bool,
  }
  impl Diagnostics {
      pub fn new(levels: LogLevels) -> Diagnostics;
      pub fn version(&self) -> u64;
      pub fn describe_source(&mut self, source: &str, summary: SourceSummary);   // bridge, once per source at start; bumps
      pub fn note_health(&mut self, source: &str, worst: Health, detail: String, at: SystemTime);
      pub fn note_polled(&mut self, source: &str, ready: usize, at: SystemTime, next: SystemTime);
      pub fn note_published(&mut self, dataset: &str);            // bumps version; sets pending_catalog_request when watchers > 0
      pub fn note_config(&mut self, diags: Vec<Diagnostic>, at: SystemTime);
      pub fn note_dropped(&mut self, total: u64);
      pub fn set_restart_required(&mut self, message: Option<String>);
      pub fn set_catalog(&mut self, snapshot: CatalogSnapshot);
      pub fn refresh_frame_hist(&mut self, hist: &FrameHistogram) -> bool;   // copies; true (and bumps) only when watchers > 0
      pub fn watch(&mut self); pub fn unwatch(&mut self); pub fn watchers(&self) -> u32;
      pub fn request_level(&mut self, target: &str, level: Level);      // sets pending_level, updates `levels`, bumps
      pub fn take_pending_level(&mut self) -> Option<(String, Level)>;
      pub fn request_overlay_toggle(&mut self); pub fn take_pending_overlay_toggle(&mut self) -> bool;
      pub fn take_pending_catalog_request(&mut self) -> bool;
      pub fn summary(&self) -> String;   // "sources 3 ok · 1 degraded · config 2 errors · 5 dropped · restart required: …"
  }
  // hash tail for the crash file (Task 6 fills it; the type lives here)
  pub struct ActionTail { hashes: [u64; 32], next: usize, len: usize }
  impl ActionTail { pub const fn new() -> ActionTail; pub fn record(&mut self, id: &str); pub fn recent(&self) -> impl Iterator<Item = u64> + '_; }
  pub fn fnv1a(s: &str) -> u64;
  ```
  Every `note_*`/`set_*` that changes state bumps `version`; a call
  that changes nothing (the same health again, the same summary) does
  **not** bump — the tile compares versions and must not rebuild on a
  no-op poll. `ShellView::diagnostics() -> &Entity<Diagnostics>`.

- [ ] **Step 1: Write the failing pure tests** in `diagnostics.rs`:

  ```rust
  #[test]
  fn a_repeated_identical_health_does_not_bump_the_version() {
      let mut d = Diagnostics::new(LogLevels::default());
      let t = SystemTime::UNIX_EPOCH;
      d.note_health("risk", Health::Ok, "".into(), t);
      let v = d.version();
      d.note_health("risk", Health::Ok, "".into(), t + Duration::from_secs(1));
      assert_eq!(d.version(), v, "no change, no rebuild");
      d.note_health("risk", Health::Degraded { reason: "x".into() }, "x".into(), t + Duration::from_secs(2));
      assert!(d.version() > v);
      assert_eq!(d.sources["risk"].history.len(), 2, "each transition recorded");
      assert_eq!(d.sources["risk"].since, t + Duration::from_secs(2));
  }
  #[test]
  fn history_is_capped_at_sixteen_transitions() { /* 20 alternating transitions → len 16, oldest dropped */ }
  #[test]
  fn the_summary_counts_sources_by_health_and_config_errors() {
      let mut d = Diagnostics::new(LogLevels::default());
      d.note_health("a", Health::Ok, "".into(), SystemTime::UNIX_EPOCH);
      d.note_health("b", Health::Degraded { reason: "r".into() }, "r".into(), SystemTime::UNIX_EPOCH);
      d.note_config(vec![Diagnostic::error(Layer::User, PathBuf::new(), "bad")], SystemTime::UNIX_EPOCH);
      d.note_dropped(5);
      assert_eq!(d.summary(), "sources 1 ok · 1 degraded · config 1 error · 5 dropped");
  }
  #[test]
  fn a_publish_requests_a_catalog_only_while_watched() {
      let mut d = Diagnostics::new(LogLevels::default());
      d.note_published("risk");
      assert!(!d.take_pending_catalog_request());
      d.watch();
      d.note_published("risk");
      assert!(d.take_pending_catalog_request());
      assert!(!d.take_pending_catalog_request(), "drained");
  }
  #[test]
  fn the_frame_histogram_is_copied_only_while_watched() {
      let mut d = Diagnostics::new(LogLevels::default());
      let mut h = FrameHistogram::new(); h.record_micros(1000);
      assert!(!d.refresh_frame_hist(&h));
      d.watch();
      assert!(d.refresh_frame_hist(&h));
      assert_eq!(d.frame_hist.count(), 1);
  }
  #[test]
  fn request_level_updates_levels_and_queues_one_persist() {
      let mut d = Diagnostics::new(LogLevels::default());
      d.request_level("ingest", Level::DEBUG);
      assert_eq!(d.levels.targets, vec![("ingest".to_string(), Level::DEBUG)]);
      assert_eq!(d.take_pending_level(), Some(("ingest".to_string(), Level::DEBUG)));
      assert_eq!(d.take_pending_level(), None);
  }
  #[test]
  fn the_action_tail_keeps_the_last_thirty_two_without_allocating() {
      let mut t = ActionTail::new();
      for i in 0..40 { t.record(&format!("a{i}")); }
      let recent: Vec<u64> = t.recent().collect();
      assert_eq!(recent.len(), 32);
      assert_eq!(recent[0], fnv1a("a8"), "oldest kept is the 9th");
      assert_eq!(*recent.last().unwrap(), fnv1a("a39"));
  }
  ```

- [ ] **Step 2: Run** — compile failure. **Step 3: Implement** the
  entity exactly per the interface (BTreeMaps, `VecDeque` caps 16 and
  256, `summary()` built with one `String` — it is called from the
  status bar's render path, so cache it: `summary_cache:
  (u64, String)` keyed on `version`, rebuilt on miss; the status bar
  gets `&str`). **Step 4: Run** — pass.

- [ ] **Step 5: Wire the shell.** `ShellView::new` creates
  `diagnostics = cx.new(|_| Diagnostics::new(services.log.as_ref().map(|l| l.levels.clone()).unwrap_or_default()))`,
  observes it (`cx.observe(&diagnostics, |this, d, cx| { … drains …; cx.notify() })`):
  the observer drains `take_pending_level` → `services.log.control.set(&levels)`
  and spawns `log_persist::persist_log_level_to_user_config(dir, target, level)`
  on the background executor (Task 5 writes that function; here, call
  it — write it in this task as a `toml_edit` edit of `app.toml`'s
  `[log]` table, mirroring `persist_slot_to_user_config`, with its own
  test on a tempdir: creates the file, adds `[log]`, preserves a
  comment); drains `take_pending_overlay_toggle` → `self.perf_overlay = !self.perf_overlay`.
  The reload tick (the 500 ms loop) calls
  `diagnostics.update(cx, |d, cx| if d.refresh_frame_hist(&this.perf) { cx.notify() })`.
  `apply_reload` and startup call `note_config(new_config.diagnostics.clone(), now)`;
  when `changed("app")` and `LogLevels::from_doc(&new)` differs from
  the entity's `levels`, call `control.set` and update the entity.
  `RestartRequired` → `set_restart_required`. Delete `data_status`,
  `set_data_status`; `status_bar` takes `diagnostics_summary` from
  `self.diagnostics.read(cx).summary()` (empty string → `None`).
  `ModuleFactory::create` gains the entity parameter; `BlotterFactory`
  and `PlaceholderFactory` accept and ignore it.

- [ ] **Step 6: Wire the bridge.** At `attach`, call `describe_source` once per `DataServiceConfig.sources` entry (paths, `priority` and `readiness` rendered with their `Debug`/label forms). Each `DataEvent` arm updates the
  entity (`Published` also still feeds the frame); `Polled` →
  `note_polled`; `Catalog` → `set_catalog` (tag-checked: the bridge
  keeps `catalog_tag: u64` and drops an outcome whose tag is older);
  dropped-events → `note_dropped`. After each entity update, if
  `take_pending_catalog_request()` → `handle.catalog(CatalogParams { key: DIAGNOSTICS_KEY, tag: next, as_of: frame.as_of() })`
  where `pub const DIAGNOSTICS_KEY: QueryKey = QueryKey(u64::MAX - 2)`
  in `shell/mod.rs` beside `PICKER_KEY`. The tile's `set_visible(true)`
  (Task 5) calls `watch()` and sets `pending_catalog_request`, so
  visibility triggers the first request through the same drain.

- [ ] **Step 7: Shell tests** (`shell/tests/diagnostics.rs`, the
  `TestAppContext` harness the other `shell/tests/*` files use): the
  status bar shows `sources 1 degraded` after `note_health`; a reload
  with a `[log]` change calls a recording `LevelControl` once with the
  new levels; `request_overlay_toggle` flips `perf_overlay` on the next
  observe; `request_level` persists `[log] ingest = "debug"` into the
  tempdir's `app.toml`. Bridge test: a `Catalog` outcome with a stale
  tag is dropped.

- [ ] **Step 8: Run** CI; commit
  `feat(shell): the Diagnostics entity, its feed and the status summary`.

- [ ] **Step 9: Harness entries** (verified by hand): the no-bump on
  identical health (`self.version += 1` unconditionally → caught by
  the first pure test); history cap (`16` → `1600`); `summary` omitting
  degraded; `pending_catalog_request` set regardless of watchers;
  `refresh_frame_hist` copying while unwatched; a stale `Catalog` tag
  accepted in the bridge; `[log]` reload not applied. Commit.

---

### Task 5: The `geode-diagnostics` module — five sections over the entity and the ring

**Files:**
- Create: `crates/geode-diagnostics/Cargo.toml`, `src/lib.rs`, `src/tile.rs`, `src/sections.rs`, `src/commands.rs`
- Modify: `Cargo.toml` (workspace member), `crates/geode-app/Cargo.toml` (dependency), `crates/geode-app/src/main.rs` (roster: `DiagnosticsFactory::new(ring)`; `mod+shift+d` — the factory registers `diagnostics::open` and `ShellView` handles it)
- Modify: `crates/geode-shell/src/shell/mod.rs` (`open_module(kind)`, `pending_kind_for_new_tile`), `shell/occupants.rs` (consume it), `shell/input.rs` (`diagnostics::open` arm), `defaults.rs` (the binding and the `diagnostics` key-context bindings)
- Test: `commands.rs` and `sections.rs` pure tests; `tile.rs` `TestAppContext` tests; `shell/tests/occupants.rs` for `open_module`

**Interfaces:**
- Consumes: `Entity<Diagnostics>`, `Entity<Frame>` (for `RequeryStats` and the as-of), `Arc<Ring>`, `Config` (for the explainer: the factory holds `Rc<RefCell<Config>>` refreshed by the bridge on reload, the way `BlotterFactory::set_views` is).
- Produces:
  ```rust
  // geode_diagnostics
  pub fn init(cx: &mut App);                                  // nothing to reclaim today; kept for symmetry with geode_blotter::init
  pub struct DiagnosticsFactory { … }
  impl DiagnosticsFactory { pub fn new(ring: Arc<Ring>, config: Config) -> DiagnosticsFactory; pub fn set_config(&self, config: Config); }
  impl ModuleFactory for DiagnosticsFactory { fn kind(&self) -> &'static str { "diagnostics" } … }
  pub const ACTIONS: &[(&str, &str)] = &[
      ("diagnostics::open", "Open diagnostics"), ("diagnostics::down", "Cursor down"), ("diagnostics::up", "Cursor up"),
      ("diagnostics::top", "Cursor to top"), ("diagnostics::bottom", "Cursor to bottom"),
      ("diagnostics::page_down", "Half page down"), ("diagnostics::page_up", "Half page up"),
      ("diagnostics::next_section", "Next section"), ("diagnostics::prev_section", "Previous section"),
      ("diagnostics::expand", "Expand"), ("diagnostics::collapse", "Collapse"),
  ];
  // commands.rs (pure)
  pub enum Command { Section(Section), Level { target: String, level: Level }, Overlay }
  pub enum Section { Sources, Data, Config, Log, Perf }
  pub fn parse(line: &str) -> Result<Command, String>;        // ":section log", ":level ingest debug", ":overlay"; errors are one line
  pub fn completions(line: &str, cursor: usize) -> Vec<String>;
  // sections.rs (pure row builders; each returns Vec<Row>)
  pub struct Row { pub text: SharedString, pub depth: u8, pub tone: Tone /* Normal, Muted, Warn, Error, Marked */, pub collapsible: Option<bool> /* Some(open) */ }
  pub fn sources_rows(d: &Diagnostics, now: SystemTime) -> Vec<Row>;
  pub fn data_rows(d: &Diagnostics, as_of: &AsOf, collapsed: &BTreeSet<String>) -> Vec<Row>;
  pub fn config_rows(d: &Diagnostics, config: &Config, filter: &str) -> Vec<Row>;
  pub fn log_rows(records: &[Record], filter: &str) -> Vec<Row>;
  pub fn perf_rows(d: &Diagnostics, requery: &RequeryStats) -> Vec<Row>;
  ```
  The `ShellView` side: `pub fn open_module(&mut self, kind: &str, window, cx)`.

- [ ] **Step 1: Crate skeleton.** `Cargo.toml` copied from
  `geode-blotter` (no bench target; `[lib] bench = false`; deps
  `geode-core`, `geode-shell`, `geode-data`, `gpui`, `gpui-component`,
  `chrono`, `toml`, `tracing`; dev-deps with `test-support` features as
  the blotter has). Add to the workspace `members`. `cargo check`.

- [ ] **Step 2: Failing pure tests** in `commands.rs`:

  ```rust
  #[test] fn section_parses_each_name_and_rejects_unknown() {
      assert!(matches!(parse("section log"), Ok(Command::Section(Section::Log))));
      assert_eq!(parse("section nope").unwrap_err(), "unknown section 'nope' (sources, data, config, log, perf)");
  }
  #[test] fn level_parses_target_and_level() {
      assert!(matches!(parse("level ingest debug"), Ok(Command::Level { .. })));
      assert_eq!(parse("level ingest loud").unwrap_err(), "unknown level 'loud' (error, warn, info, debug, trace)");
      assert_eq!(parse("level nope info").unwrap_err(), "unknown target 'nope' (ingest, query, config, session, shell, theme)");
  }
  #[test] fn completions_offer_sections_then_targets_then_levels() {
      assert_eq!(completions("section l", 9), vec!["section log"]);
      assert_eq!(completions("level in", 8), vec!["level ingest"]);
      assert_eq!(completions("level ingest d", 14), vec!["level ingest debug"]);
  }
  ```
  and in `sections.rs` (fixtures built from `Diagnostics::new` plus
  `note_*` calls):

  ```rust
  #[test] fn sources_are_sorted_worst_first_with_their_detail() { /* Failed before Degraded before Ok; row text contains name, label, detail, "since", "last poll", "next poll" */ }
  #[test] fn data_rows_mark_the_resolved_generation_under_an_as_of() { /* the row for gen 1 carries Tone::Marked when resolved_gen == Some(1); collapsed dataset shows one row */ }
  #[test] fn config_rows_list_diagnostics_then_the_explainer_filtered_by_path() { /* "[user] app.toml: bad" then "theme.name = \"Gruvbox Dark\"  [user]" via Config::explain; filter "theme" keeps only matching leaves */ }
  #[test] fn log_rows_filter_by_target_or_level_text() { /* "ingest" keeps geode::ingest rows; "WARN" keeps warn rows */ }
  #[test] fn perf_rows_carry_p50_p95_max_and_dropped() { /* uses FrameHistogram::percentile_micros and RequeryStats::last */ }
  ```

- [ ] **Step 3: Run** — compile failure. **Step 4: Implement** the
  pure modules. `Row.text` is built once per rebuild; the renderer
  clones `SharedString`s. Time formatting: local clock, `HH:MM:SS`
  (the frame's convention — reuse `geode_core::query`'s local-time
  helpers if exported, else `chrono::Local`). **Step 5: Run** — pass.

- [ ] **Step 6: The tile.** `DiagnosticsTile { tile: TileId, frame,
  diagnostics, ring, config, section, cursor: usize, collapsed:
  BTreeSet<String>, filter: String, follow: bool, since: u64, records:
  Vec<Record>, rows: Vec<Row>, seen: (u64 /* diagnostics version */,
  FrameVersions, u64 /* ring seq */), visible: bool, scroll:
  UniformListScrollHandle }`. `new` observes `diagnostics` and `frame`;
  each observe compares versions and calls `rebuild(cx)` only on
  change. `rebuild` picks the section's row builder; for `Log` it calls
  `ring.drain_since(self.since, &mut self.records)` **only when
  `ring.latest_seq() > self.since`**, appends to a bounded local
  `VecDeque<Record>` (4,096) and sets `since = latest_seq`; `follow`
  keeps the cursor on the last row unless the user moved it. Render:
  a header (`diagnostics · <section> · [ ] to switch`) plus
  `uniform_list(cx.entity(), "rows", rows.len(), |this, range, _w, cx| …)`
  painting `Row`s in `fonts::MONO`, cursor row highlighted, `Tone`
  mapped to theme colours. `key_context` = `KeyContext::new("diagnostics").pair("section", name).counts()`.
  `dispatch` handles the `ACTIONS` (vim motions with counts; `[`/`]`
  cycle sections; `zo`/`zc` on `Data`). `command` parses via
  `commands::parse`: `Section` switches; `Level` calls
  `diagnostics.update(|d, cx| { d.request_level(&target, level); cx.notify() })`;
  `Overlay` calls `request_overlay_toggle`. `find(FindEvent::Changed(s))`
  sets `filter` and rebuilds; `Committed` keeps it; `Cancelled` clears.
  `set_visible(true)` → `diagnostics.update(|d| d.watch())` and
  `request_catalog`; `false` → `unwatch`. `serialize` writes
  `section` and `filter`; `create(restored)` reads them back.
  `deliver` ignores `QueryOutcome` (this tile never queries).

- [ ] **Step 7: `TestAppContext` tests** in `tile.rs`, harness copied
  from the blotter's `open(cx)` (the host owns `frame`, `diagnostics`,
  `ring`, the tile):
  - `a_health_event_shows_in_the_sources_section`: `note_health` then
    draw → a row's text contains `risk: degraded — reason`.
  - `level_ingest_debug_changes_the_entity_and_queues_a_persist`:
    `command("level ingest debug")` → `diagnostics.levels` updated,
    `take_pending_level() == Some(("ingest", DEBUG))`.
  - `the_log_section_follows_the_tail_until_the_cursor_moves`: push
    three records, draw, cursor at last; `dispatch(up)`; push one more;
    cursor unchanged and `follow == false`.
  - `switching_sections_and_serialising_round_trips`.
  - `an_unchanged_entity_does_not_rebuild_rows`: two draws without a
    version change → `rebuild` count (a `#[cfg(test)]` counter) is 1.
  - `visibility_watches_and_requests_a_catalog`: `set_visible(true)` →
    `watchers() == 1` and `take_pending_catalog_request()`.

- [ ] **Step 8: Opening the tile.** In `ShellView`:
  `open_module(kind)`: if any occupant in the focused workspace has
  `kind` → focus its tile; else `split` the focused tile (the same
  path `ctrl+v` takes), set `pending_kind_for_new_tile = Some(kind)`,
  and `sync_occupants` uses that kind's factory for the one new tile
  (falling back to the default kind if no factory), then clears it.
  `diagnostics::open` in `dispatch` calls `open_module("diagnostics")`;
  `defaults.rs` binds `"mod+shift+d" = "diagnostics::open"` and, in
  the `diagnostics` context, `j/k/gg/G/ctrl+d/ctrl+u/[/]/zo/zc` to the
  module actions (copy the blotter's context block). Clicking the
  status bar's summary dispatches `diagnostics::open` too. Test in
  `shell/tests/occupants.rs`: `open_module("diagnostics")` twice yields
  one diagnostics tile, focused; with no factory of that kind the
  default kind is used and a `warn!` is logged.

- [ ] **Step 9: `main.rs`**: `roster.add(Box::new(DiagnosticsFactory::new(ring.clone(), config.clone())))`
  (the ring from Task 2's setup; `set_config` on reload from the
  bridge's `ConfigReloaded` arm). Run `cargo run -p geode-app -- --demo`
  headlessly? No — record in the report that a display check is
  needed: `mod+shift+d` opens the tile, `]` cycles, `:level ingest
  debug` shows in the log section and lands in `app.toml`.

- [ ] **Step 10: Run** CI; commit
  `feat(diagnostics): the module — five sections, :section, :level, :overlay`.

- [ ] **Step 11: Harness entries** (verified by hand): sections sorted
  ok-first (caught by the sort test); the resolved marker on the wrong
  generation; the log filter matching neither target nor level;
  `follow` never cleared (caught by the tail test); rebuild on every
  observe (the counter test); `open_module` opening a second tile.
  Commit.

---

### Task 6: Panic boundaries — the ingest error event, the crash file, the action tail

**Files:**
- Modify: `crates/geode-data/src/ingest/runner.rs` (the panic arm logs at `error` with file and payload)
- Create: `crates/geode-app/src/crash.rs` (`install_panic_hook`, `write_crash_file`, `trim_log_files` from Task 2)
- Modify: `crates/geode-app/src/main.rs` (install after the subscriber)
- Modify: `crates/geode-shell/src/actions.rs` (`ActionRegistry::name_of_hash`, the `hash → id` map filled at `register`), `shell/input.rs` (`dispatch` records into the tail), `shell/mod.rs` (`action_tail: Arc<Mutex<ActionTail>>` in `ShellServices`, created in `main.rs`)
- Test: `runner.rs` (the panic path's event carries the file), `crash.rs` (the file's contents on a tempdir), `actions.rs`, `shell/tests/input.rs`

**Interfaces:**
- Consumes: `ActionTail`/`fnv1a` from Task 4; the ring.
- Produces:
  ```rust
  // geode_app::crash
  pub fn install_panic_hook(dir: Option<PathBuf>, ring: Arc<Ring>, tail: Arc<Mutex<ActionTail>>, names: Arc<dyn Fn(u64) -> Option<String> + Send + Sync>);
  pub fn write_crash_file(dir: &Path, at: SystemTime, message: &str, location: Option<&str>, records: &[Record], actions: &[String]) -> std::io::Result<PathBuf>;   // crash-<YYYYMMDD-HHMMSS>.log
  pub fn trim_log_files(dir: &Path, keep: usize);
  // geode_shell::actions
  impl ActionRegistry { pub fn name_of_hash(&self, h: u64) -> Option<&str>; }
  ```

- [ ] **Step 1: Failing tests.** `runner.rs`: a `load_file` that
  panics (inject via a `#[cfg(test)]` hook the runner already uses for
  the boundary test, or a CSV path that makes `load_file` panic —
  read the existing `a_failing_item_degrades_and_the_runner_keeps_going`
  and extend it) produces `IngestEvent::Failed` whose `reason` names
  the file and the panic payload (`"ingest task panicked at <path>:
  <payload>"`). `crash.rs`: `write_crash_file` on a tempdir writes a
  file whose text contains the message, the location, every record's
  message in order, and the action names in order; `trim_log_files`
  with nine `geode.2026-09-0N.log` files keeps the newest seven.
  `actions.rs`: `register` then `name_of_hash(fnv1a(id)) == Some(id)`.
  `shell/tests/input.rs`: dispatching three actions leaves their hashes
  in the tail in order.

- [ ] **Step 2: Run** — failures. **Step 3: Implement.** The runner's
  panic arm: extract the payload (`&str` or `String` downcast, else
  `"<non-string panic>"`), `tracing::error!(target: "geode::ingest",
  file = %path.display(), "ingest task panicked: {payload}")`, and put
  file and payload in `Failed.reason`. The hook: `std::panic::take_hook()`
  saved as `previous`; the new hook formats message and location,
  drains the ring (`drain_since(0, &mut Vec::with_capacity(ring.capacity()))`
  — allocation is fine on the panic path), resolves the tail through
  `names`, calls `write_crash_file`, logs `error!(target: "geode::shell",
  "crash file written to {path}")`, then `previous(info)`. If `dir` is
  `None`, it only logs. The tail: `dispatch` calls
  `tail.lock().record(&action.0)` before matching — a `Mutex` lock per
  keypress, no allocation. `ActionRegistry::register` inserts
  `(fnv1a(&id.0), id.0.clone())` into a `HashMap<u64, String>`; the
  closure passed to the hook reads a snapshot `Arc<RwLock<HashMap>>`
  shared with the registry (register on reload updates it).

- [ ] **Step 4: Run** — pass. CI. Commit
  `feat(app): crash file with the log tail and the last actions; ingest panics name the file`.

- [ ] **Step 5: Harness entries** (verified by hand): the panic
  payload dropped from `reason` (caught by the runner test); the crash
  file written without records (caught by the crash test); the tail
  not recorded on dispatch (caught by the input test); `trim_log_files`
  keeping `keep + 1`. Commit.

---

### Task 7: Docs, the spec's as-built notes, and the branch-end harness

**Files:**
- Modify: `CLAUDE.md` (a "Phase 4b is done" paragraph: targets and levels, `[log]`, the ring, the entity, `mod+shift+d`, the crash file, the `open_module` door, the harness count)
- Modify: `docs/perf.md` (a "Phase 4b" section: the diagnostics tile open with the log following must not move the frame histogram's p95 — the recipe, measured in `--demo` with the overlay by the user, template rows if no display; the ring's reader allocation test as the recorded contract)
- Modify: `docs/superpowers/specs/2026-09-06-geode-phase-4-frame-features-design.md` §4 (as-built notes: the histogram copy on the reload tick; `Polled`; the log directory; `Failed` not `Degraded` for an ingest panic; hashes in the tail; `open_module`; the `RollingFileAppender` name format — and, from Task 2's fix round 1 (MIN-7), that the format's date rolls on the UTC date, not the trader's local one: Phase 4a's "times are the trader's local clock throughout" ruling governs every *displayed* time, not the log file's own name)
- Modify: `scripts/mutation-check.sh` (reconcile every entry the branch added; count)

- [ ] **Step 1:** Write the three documents. **Step 2:** `grep -c
  'run_mutation "' scripts/mutation-check.sh` equals `CLAUDE.md`'s
  count. **Step 3:** CI. **Step 4:** Commit `docs: Phase 4b as built`.
  The controller then runs the unfiltered harness detached and the
  final whole-branch review.

---

## Self-review

**Spec coverage.** §4.1 targets, levels, the UI-thread rule, the
subscriber order, `reload_handle` and `ring` into `ShellServices` —
Task 2. §4.2 the ring's API and allocation rule — Task 2 (the two-writer
and no-allocation tests pin it). §4.3 files, seven-file cap, `[log]`,
reload and `:level` through the write door — Tasks 2, 4, 5. §4.4 the
entity's fields, version bump on write only, every event landing there,
`set_data_status` deleted, the status bar reading a summary — Task 4.
§4.5 `CatalogParams`/`CatalogSnapshot`, built on the data thread from
`file_generations`, `pragma_database_size` and per-table info, requested
on visibility and after each publish while visible, coalesced by tag,
functions verified by execution — Task 3 (verified table above) and
Task 4 (the request drain). §4.6 the module, `diagnostics::open` on
`mod+shift+d`, the five sections with their motions, `/` filtering,
rebuild only on version change, `zo`/`zc`, `:level` persisting,
`:overlay`, no `DataTable` — Task 5. §4.7 the ingest boundary (existing,
with the `error` event added), the crash file with ring and 32 actions,
the allocation-free tail — Task 6. §4.8 the status bar summary and click
— Tasks 4 and 5. §7 "Other contracts" — the rebuild-on-version rule
(Tasks 4, 5), the ring's reader (Task 2), the p95 measurement (Task 7).
§8's pure tests for the ring and the ingest boundary — Tasks 2 and 6;
the module tests named in §8 ("a Health event shows in the sources
section"; "`:level ingest debug` changes the filter and persists") —
Task 5. §9 crate layout — Task 5's crate with the stated dependencies.
Deviations are all recorded under "Rulings taken while planning" and
land in the spec's as-built notes in Task 7. Gap found and closed while
reviewing: §4.6's `sources` section shows "path, priority, readiness
rule", which come from `SourceSpec`, which the shell does not hold —
Task 4's `SourceState.spec: Option<SourceSummary>` is filled by the
bridge from `DataServiceConfig.sources` at `attach`, and Task 5's
`sources_rows` prints it under the health line.

**Placeholder scan.** No "TBD"/"TODO"; every code step has code or an
exact description with names; Task 5's row-builder tests are described
by their assertions rather than full bodies — acceptable because the
fixtures are the entity's own `note_*` calls, but the implementer must
write full assertions, not comments. Task 2 step 6 depends on
`tracing-appender`'s builder API; the implementer verifies the exact
method names against the crate's docs for the resolved version.

**Type consistency.** `Diagnostics::request_level(target: &str, level: Level)`
in Task 4 matches `Command::Level { target: String, level: Level }` in
Task 5; `take_pending_catalog_request` is named identically in Tasks 4
and 5; `DIAGNOSTICS_KEY` is defined in Task 4 and used only by the
bridge; `ActionTail`/`fnv1a` live in `geode_shell::diagnostics` (Task 4)
and are consumed by Task 6; `LogServices { ring, control, levels }` is
defined in Task 2 and read in Task 4; `ModuleFactory::create`'s new
parameter is added in Task 4 and used by Task 5's factory.
