# Line Pricer Part 4 (Storage) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Sheets persist in DuckDB. The `pricer_sheets` local document dataset ships in the builtin config layer; a `DataHandle`-backed `SheetStore` loads (a document request answered through `Delivery::Query`) and saves (a local publish whose success or failure reaches the tile that saved); `:e`, `:new`, `:name` and `:rm` (a confirmed forget of every generation) work; local datasets keep at most 200 generations per document.

**Architecture:** The data tier gains one writer-thread operation (`forget` a document key) and structured outcomes for local writes (`LocalPublished`/`LocalPublishFailed`/`Forgotten`/`ForgetFailed`), plus a post-publish retention sweep for local datasets. The pricer core gains a snapshot decoder (`rows_from_snapshot`) — the inverse of `to_rows` over a document query's answer. The pricer's `SheetStore` seam is reshaped so a store can address its answer to the asking tile (`load(name, key, tag)`), and gains `forget` and a known-names view. The bridge routes local-write outcomes for `pricer_sheets` to the pricer factory by sheet name (no new `Delivery` variant, no shell change). The tile decodes its load answer in its `Delivery::Query` arm and calls the existing `loaded(..)`.

**Tech Stack:** Rust 2024, DuckDB (via `geode-data`'s store), gpui + gpui-component (pinned), `geode_core::{snapshot, document, schema, query}`.

**Spec:** `docs/superpowers/specs/2026-09-19-geode-line-pricer-design.md` — §7 (storage: seam, dataset, write-behind, names/open set/`:rm`), §8.6 (the four verbs), §14 part 4, and §18 (as built through Part 3, especially its "Part 4 obligations"). Read §7 and §18 before starting.

## Decisions made in planning

1. **`SheetStore` is reshaped to address answers.** `load(&self, name, key: QueryKey, tag: u64) -> Loaded` (the DuckDB store submits a `DocumentParams` with that key/tag and answers `Pending`, or `Refused` when the request channel refuses); `save(&self, name, rows) -> bool` (queued, not stored); `forget(&self, name) -> bool` (queued); `names(&self) -> Vec<String>` and `contains(&self, name) -> bool` read a known-names cache. `Loaded` gains `Refused`. `MemorySheetStore` implements the new trait (answers synchronously as today) and stays the tests' fake.
2. **The tile keeps its own load tag**, separate from the pricing tag, and ignores a `Delivery::Query` whose tag is not its latest load. Both use the tile's `QueryKey(tile id)`; the query pool and the pricing worker are separate lanes, so they never supersede each other.
3. **A hide cancels by key, which cancels a pending load too** (`DataHandle::cancel` reaches both lanes). So a tile still `loading` on show re-submits its load under a fresh tag.
4. **Loads are always `AsOf::Live`.** A sheet does not follow the frame's as-of; §1.1's "browsable as-of" is deferred (no reader for it exists).
5. **The document answer is decoded in the core** (`core::storage::rows_from_snapshot(name, &Snapshot) -> Result<Option<DocumentRows>, String>`): `None` for a zero-row answer ("Missing"); utf8 through `Snapshot::text_value` (plain or dictionary), numbers through `i64_column`/`f64_column`; attributes read from row 0; rows passed in answer order (`from_rows` orders by `order`). It produces exactly the `Column`/`Value` variants `from_rows` requires, driven by the declaration's column list, not by the snapshot's.
6. **Save outcomes reach the tile by sheet name through the bridge.** `geode-data` emits `DataEvent::LocalPublished { dataset, batch, gen_id }` (in addition to today's `Published`, which diagnostics keeps using) and `DataEvent::LocalPublishFailed { dataset, batch, reason }` (in addition to today's error `Diagnostics`). The bridge routes both for `pricer_sheets` to `PricerFactory::save_answered(sheet, Result<(), String>)`, which finds the tile holding that name (the open set) and updates it. No new `Delivery` variant; no shell change.
7. **Dirty semantics become asynchronous.** `save()` returning `true` means *queued*: the tile records the save as in flight. `LocalPublished` confirms it (clears `dirty` and any save notice); `LocalPublishFailed` restores `dirty` and paints the save notice with the reason; the next edit burst retries (spec §7.3). A close with a save still unconfirmed does nothing extra — the write is already queued on the writer.
8. **Forget is a writer-thread operation.** `store::document::forget_document(store, ds, batch)` deletes the batch's live and archive rows and its `generations` (and `file_generations`/`file_books`) rows in one transaction, then refreshes the dataset's categorical ENUMs. It runs as a new `Work::Forget` in the ingest runner's documents FIFO, so a forget queued after a save of the same sheet deletes that save too (intended). Local datasets only (a forget of any other dataset is refused with an error diagnostic, never run). Outcome events `DataEvent::Forgotten { dataset, batch }` / `DataEvent::ForgetFailed { dataset, batch, reason }`.
9. **Retention for local datasets runs on the writer after each local publish:** `retention::sweep(conn, ds, pairs, &RetentionPolicy { keep_generations: Some(200), keep_age: None }, now)` for that dataset only (spec §7.2). The sweeper for other datasets stays unwired (the separate decision it was).
10. **`sheet` is declared `categorical = false`.** A text dimension is categorical by default, which would put sheet names into the frame picker and groupings and rebuild an ENUM on every autosave. Sheets are not a scope dimension. (Verify `validate_document` accepts a non-categorical utf8 key dimension; if it does not, say so and stop — that is a spec-level question.)
11. **The declaration is frozen.** Tables are `CREATE TABLE IF NOT EXISTS` and publishes insert positionally, so once a database holds `pricer_sheets` its column list and order cannot change without a migration. Record this in the crate README and the declaration's doc comment; no migration machinery now.
12. **Known names come from the diagnostics catalog plus this session's own writes.** The production store holds a shared `known: RefCell<BTreeSet<String>>`: seeded and refreshed from `Diagnostics.catalog`'s `pricer_sheets` partitions (the factory observes the diagnostics entity and pushes names in), plus every name it saved, minus every name it forgot. The factory asks for a catalog explicitly (`Diagnostics::request_catalog` + notify) when it is created and none is held. Known limitation: a tile created before the first catalog lands may pick an `untitled-N` that already has a document; its first save then adds a generation to that document (history kept, nothing lost) — recorded, not engineered around.
13. **`:e <sheet>`** saves the current sheet now if dirty, releases its name, claims the new one (refused `sheet 'x' is open in another tile` if open elsewhere), resets the undo history and expansion, and loads (`Pending` → `loading…`). `:e` of the tile's own current name is a no-op. **`:new`** does the same into the next `untitled-N`, with no load. **`:name <new>`** is refused if `<new>` is open or known (`sheet 'x' already exists`); otherwise it claims the new name, saves the sheet under it now, and forgets the old name after that save is confirmed (so a failed save never loses the old document). **`:rm <sheet>`** is refused for an open name (the current tile's included: "close it or `:e` another sheet first"), and for an unknown name; otherwise it arms a confirm.
14. **The `:rm` confirm copies the market-data upload confirm**: a focused prompt in the header (`remove sheet 'x' and all its history? (y/n)`), the tile in `insert` mode while armed (the confirm holds the keyboard), every key consumed, bare `y` confirms, anything else cancels, focus leaving the tile or a pointer press cancels, blur-then-drop. `y` calls `store.forget(name)`; `Forgotten` removes it from the known names.
15. **Header precedence while loading:** a load starting clears a standing refusal streak (nothing is submitted while loading, and `loaded()` resubmits), so `loading…` always shows during a load (closes the §18 obligation).
16. **Catalogue underlyings for the typeahead stay deferred:** no source of pricing underlyings exists yet.
17. **The builtin `datasets` doc.** `builtin_layer` pushes `LayerDoc::builtin("datasets", PRICER_SHEETS_DECLARATION)` before the demo layer, so a desk/user/demo `datasets` doc unions with it by dataset name (`atomic_depth("datasets") == 1`). Consequence: `data_setup`'s `datasets` gate always passes; the bridge still requires a `views` doc. Audit every `config.doc("datasets").is_some()` branch in `geode-shell` for a behaviour change now that the doc always exists (list them in the report; change none unless one is now wrong).

## Global Constraints

- `geode-pricer` never depends on `geode-pricing` or a sibling module; it reaches storage only through its `SheetStore` seam and the data tier's request doors.
- Only `geode-data` owns DuckDB and the writer; forget and the retention sweep run on the ingest writer thread, serialized with publishes.
- A publish or forget to a non-local dataset is refused with an error diagnostic, never run.
- The UI thread never waits: every store call returns at once (`bool`/`Loaded`); answers arrive as events.
- Incorrect narrowing or a plausible wrong sheet is worse than an explicit error: a decode failure blocks saves (the existing `block_saves` path), never installs a half-sheet.
- `:` commands change only this tile.
- Every existing test stays green; the tile's test harness keeps using `MemorySheetStore`.
- Cargo in the foreground; one build at a time; gate: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo check -p geode-shell --features test-support --all-targets`, `zsh scripts/mutation-check.sh --anchors-only`. Mutation entries for every new correctness contract; filtered runs only.
- Commit trailer `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

---

### Task 1: Data tier — forget, local-write outcomes, local retention

**Files:** `crates/geode-data/src/store/document.rs` (forget), `crates/geode-data/src/ingest/runner.rs` (`Work::Forget`, events, post-publish sweep), `crates/geode-data/src/handle.rs` (`Request::Forget`, `DataHandle::forget`), `crates/geode-data/src/service.rs` (gate, `DataEvent` variants, sink mapping), `crates/geode-data/src/store/retention.rs` (if a helper is needed), README.

**Interfaces produced:**
```rust
// geode_data::store::document
pub fn forget_document(store: &Store, ds: &DatasetSpec, batch: &str) -> Result<usize, StoreError>; // rows deleted
// geode_data
pub struct LocalForget { pub dataset: String, pub key: Vec<String> }
impl DataHandle { pub fn forget(&self, forget: LocalForget) -> bool; } // false = not admitted
pub enum DataEvent { …,
    LocalPublished { dataset: String, batch: String, gen_id: i64 },
    LocalPublishFailed { dataset: String, batch: String, reason: String },
    Forgotten { dataset: String, batch: String },
    ForgetFailed { dataset: String, batch: String, reason: String },
}
```

- [ ] **Tests first** (in `geode-data`, real temp store):
  - `forget_document` removes every live and archive row of one batch, its `generations` rows and its `file_generations` rows, leaves another batch of the same dataset untouched, and the catalog then lists only the other batch; `assert_generations_match_tables` (or the store's equivalent consistency check) still holds.
  - A forget queued after a save of the same key (both through the runner) leaves no document.
  - `DataHandle::forget` to a non-local dataset emits an error `Diagnostics` and runs nothing.
  - A successful local publish emits `LocalPublished` with the batch and gen id AND still emits `Published`; a failing one (e.g. rows that fail `validate`) emits `LocalPublishFailed` with the reason AND the existing error diagnostic.
  - After 205 local publishes of one key, the archive holds at most 200 generations for it (live + archive ≤ 201); a non-local dataset's publishes are never swept.
- [ ] Implement per decisions 6, 8, 9. The forget SQL (inside one transaction on the writer connection; identifiers quoted as every document path does):
```sql
delete from "{live}"    where batch = ?;
delete from "{archive}" where batch = ?;
delete from generations where dataset = ? and batch = ?;
delete from file_books where file_id in (select file_id from file_generations where dataset = ? and batch = ?);
delete from file_generations where dataset = ? and batch = ?;
```
  then `ddl::refresh_enum` for each `ddl::categorical_columns(ds)`. Check the real table/column names against `store/catalog.rs` DDL before writing.
- [ ] Gate, commit.

### Task 2: Pricer core — snapshot decoder and the frozen, non-categorical declaration

**Files:** `crates/geode-pricer/src/core/storage.rs`, `crates/geode-pricer/Cargo.toml` (dev-dep on `geode-data` test-support already present), README.

**Interfaces produced:**
```rust
pub fn rows_from_snapshot(name: &str, snapshot: &geode_core::snapshot::Snapshot) -> Result<Option<DocumentRows>, String>;
```

- [ ] **Tests first:**
  - Round trip through the REAL store: build sheets covering every template, both variants, every shift state and spot overrides (reuse the Part 2 storage fixtures); publish each via `geode_data::store::document::publish_document` (or through a `DataService` with the declaration) into a temp store; read it back with the document query (`compile_document` + execute, or `DataService::document` answered in-test); `rows_from_snapshot` then `from_rows` equals the original sheet's `to_rows` → `from_rows` (ids, order, kinds, shifts, attributes).
  - A zero-row answer decodes to `Ok(None)`.
  - A snapshot missing a declared column, or with a wrong-typed column, is `Err` naming the column (never a half-sheet).
  - The declaration parses with no diagnostics and `sheet` is NOT categorical (`SchemaSpec::from_doc` → `categorical_columns()` excludes `sheet`).
- [ ] Implement per decision 5; set `categorical = false` on `sheet` (decision 10 — stop and report if the validator refuses it); add the freeze note (decision 11) to the declaration's doc comment.
- [ ] Gate, commit.

### Task 3: The store seam and the DuckDB store

**Files:** `crates/geode-pricer/src/store.rs`, `crates/geode-pricer/src/tile.rs` (call sites only), tests.

**Interfaces produced:**
```rust
pub enum Loaded { Rows(DocumentRows), Missing, Pending, Refused }
pub trait SheetStore {
    fn load(&self, name: &str, key: QueryKey, tag: u64) -> Loaded;
    fn save(&self, name: &str, rows: DocumentRows) -> bool;
    fn forget(&self, name: &str) -> bool;
    fn names(&self) -> Vec<String>;
    fn contains(&self, name: &str) -> bool;
}
pub struct DuckSheetStore { /* data: DataHandle, known: Rc<RefCell<BTreeSet<String>>> */ }
impl DuckSheetStore {
    pub fn new(data: DataHandle) -> Self;
    pub fn set_known(&self, names: impl IntoIterator<Item = String>);   // from the catalog
    pub fn note_saved(&self, name: &str);
    pub fn note_forgotten(&self, name: &str);
}
```
- [ ] **Tests first:** `DuckSheetStore` over `DataHandle::for_tests()`: `load` submits `Request::Document` with the pricer dataset, `document_key == [name]`, the given key/tag and `AsOf::Live`, answering `Pending`; a closed channel answers `Refused`; `save` submits `Request::Publish` with the dataset and rows; `forget` submits `Request::Forget`; `contains`/`names` reflect `set_known` ∪ saved − forgotten. `MemorySheetStore` passes the same trait-level tests it did (adapted to the new signatures; `forget` removes the entry; `names` lists them).
- [ ] Implement; update the tile's call sites mechanically (`load(name, QueryKey(id), load_tag)` — the tag field lands in Task 4; pass `0` here and note it).
- [ ] Gate, commit.

### Task 4: The tile's load, save and name lifecycle

**Files:** `crates/geode-pricer/src/tile.rs`, `crates/geode-pricer/src/content.rs`, header copy as needed.

- [ ] **Tests first** (tile harness, `MemorySheetStore` in pending mode plus direct `Delivery::Query` injection with a snapshot built by publishing into a real temp store in a helper, or by a test-only snapshot builder in `core::storage` tests if the harness cannot reach the store — say which):
  - A restored tile with a pending store submits nothing extra, paints `loading…`, and a `Delivery::Query` with the latest load tag installs the sheet (decoded), reprices, and keeps the held cursor/expansion; an older tag is ignored; `Err` blocks saves (existing path).
  - Hide during a pending load, then show: the load is re-submitted under a fresh tag; only the new tag's answer installs.
  - A load starting clears a refusal streak: `loading…` shows, not `REFUSED` (decision 15).
  - `Loaded::Refused` at restore: the tile opens with a notice naming the refusal and saves blocked (a refused read must not be overwritten by the fallback).
  - Save lifecycle through `PricerFactory::save_answered`: a queued save leaves `dirty` pending; `Ok` clears it and any save notice; `Err(reason)` restores `dirty`, paints the save notice with the reason, and the next edit burst retries; an answer for a name no tile holds is ignored.
- [ ] Implement per decisions 2, 3, 7, 15: `load_tag: u64`, the `Delivery::Query` arm (`content.rs`) → `rows_from_snapshot` → `loaded(..)`; `PricerFactory::save_answered(&self, sheet: &str, answer: Result<(), String>, cx: &mut App)` and `forget_answered(&self, sheet, answer, cx)` (the latter feeds Task 5's `:name`/`:rm`).
- [ ] Gate, commit.

### Task 5: Known names, `:e`, `:new`, `:name`, `:rm` and the confirm

**Files:** `crates/geode-pricer/src/{tile.rs,content.rs,core/commands.rs,header.rs,popup.rs if the menu gains rows}`.

- [ ] **Tests first:**
  - Known names: the factory seeds its store's known names from a `Diagnostics` catalog holding `pricer_sheets` partitions (build a `CatalogSnapshot` in-test and `set_catalog` it), and a later catalog replaces them; `untitled()` skips known names; `:e ` completions list the known names (sorted, minus nothing — the shell ranks); a factory created with no catalog asks for one (`request_catalog`) exactly once.
  - `:e other` from a dirty sheet: saves the current sheet first, releases its name (another tile can now `:e` it), claims `other`, loads it (`loading…`), clears undo; `:e` of a name open in another tile is refused `sheet 'other' is open in another tile`; `:e` of the current name is a no-op.
  - `:new`: next `untitled-N`, empty, no load, undo cleared.
  - `:name fresh`: refused if open or known; otherwise the title changes at once, the sheet is saved under `fresh`, and only after that save is confirmed is the old name forgotten; a failed save leaves the old name's document alone and restores the old name? — NO: keep the new name on the tile (the user asked for it) and do not forget the old document; paint the save notice. (State this rule in the doc comment.)
  - `:rm`: refused for an open name (current tile included) and for an unknown one; otherwise arms the confirm (header prompt, mode `insert`, focused); `y` forgets (`Request::Forget` submitted) and `Forgotten` removes the name from known names; `n`/`escape`/any key/pointer press/focus leaving cancels with a footer `sheet not removed`; blur-then-drop on every closer.
  - The colon sweep (`every_colon_command_leaves_the_frame_alone`) covers the four verbs (remove them from `NOT_BUILT`; that constant may become empty — delete it if so).
- [ ] Implement per decisions 12, 13, 14. Copy the confirm from `geode-marketdata/src/tile.rs` `arm/confirm_key/cancel_upload_on_pointer/disarm_upload` (~1380-1500) and its header prompt; keep names pricer-local.
- [ ] Gate, commit.

### Task 6: App wiring

**Files:** `crates/geode-app/src/main.rs` (`builtin_layer`), `crates/geode-app/src/bridge.rs` (store, routing, catalog seeding), tests.

- [ ] **Tests first:**
  - `builtin_layer` carries a `datasets` doc whose `pricer_sheets` parses clean and is `local`; a demo/desk `datasets` doc unions with it (both datasets present).
  - The bridge routes `LocalPublished`/`LocalPublishFailed` for `pricer_sheets` to the pricer factory (`save_answered`) and `Forgotten`/`ForgetFailed` (`forget_answered`), and ignores other datasets.
  - End-to-end with a REAL temp `DataService` (not `for_tests`): a pricer tile types a line, the idle save publishes, a new tile restoring the same sheet name loads it back from DuckDB with the same line (the proof Part 4 exists). If a full shell window with a real service is impractical in `geode-app` tests, do it at the `geode-pricer` level with a real `DataService` and the factory directly — say which.
  - A local publish still leaves `FrameVersions.data` unchanged (existing test stays green).
- [ ] Implement per decisions 6, 12, 17; replace `MemorySheetStore` with `DuckSheetStore` in `start`; seed known names from the catalog through the factory. Audit and list the `config.doc("datasets").is_some()` branches (decision 17).
- [ ] Gate, commit.

### Task 7: Docs, harness, spec as-built

- [ ] Mutation entries: forget deletes only its batch; the non-local forget gate; the local sweep keeps 200; the decoder's zero-row → `None`; the load-tag filter; resubmit-on-show while loading; save confirmation clears `dirty` only on `Ok`; `:name` forgets the old name only after the confirmed save; `:rm` refuses an open name; the confirm consumes every key. `--anchors-only` clean; each entry caught by name.
- [ ] `docs/current/features.md` (persistence section: DuckDB, generations kept, the four verbs, the known-names limitation, the frozen declaration), `docs/current/data-path.md` (local forget, local retention, the local-write outcome events), crate READMEs (`geode-pricer`, `geode-data`), spec §19 "As built (Part 4)" with these decisions, deviations and the obligations for any later slice (as-of browsing, catalogue underlyings, sign colouring, vim-style ambiguous keys).
- [ ] Final gate, commit.

## Self-review notes

- Spec coverage: §7.1 seam (Task 3, reshaped per decision 1), §7.2 dataset + retention (Tasks 1, 2, 6), §7.3 write-behind with failure events (Tasks 1, 4), §7.4 names/restore/open set/`:rm` with forget (Tasks 1, 4, 5), §8.6 verbs (Task 5), §18 obligations: DuckDB store + `Delivery::Query` arm (Tasks 3–4), the four verbs (Task 5), builtin declaration (Task 6), `keep_generations` (Task 1), catalogue underlyings (deferred, decision 16), refusal-vs-loading precedence (decision 15).
- Risks: the real-store round trip (Task 2) is the load-bearing proof that `to_rows` → DuckDB → snapshot → `from_rows` is lossless; the end-to-end test (Task 6) is the proof the wiring works. `categorical = false` on a key dimension is the one validator question that could stop the plan (decision 10).
