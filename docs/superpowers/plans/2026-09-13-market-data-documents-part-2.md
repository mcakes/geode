# Market-Data Documents, Part 2 — The Adapter Tier, Document Kinds and the Demo Bus

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A subscribed source declared in `sources.toml` receives whole documents from an adapter, parses each through a typed document kind, coalesces per key, and publishes them as generations of a document dataset — end to end for CVI parameters over an in-process channel adapter that is both the test fixture and `--demo`'s bus.

**Architecture:** Three new seams, each pure where it can be. `geode_core::document::DocumentKind` is the parser/writer contract; `geode-documents` is a new pure crate implementing it for CVI over `quick-xml`; `geode_data::adapter` holds the `Subscription`/`Egress`/`Adapter` traits, the registry, the bounded `MessageSink`, and `ChannelAdapter` (no sockets). The ingest runner gains a document queue popped ahead of files, and a per-source receiver thread (`ingest::subscribe`) drains the sink through parse → `Coalescer` → `IngestHandle::submit_document`. `DataService::open` partitions sources by adapter: directory sources still go to the scheduler; subscribed ones get a receiver. The demo generator produces `DocumentRows`; the app writes them through the CVI kind and pushes them on the channel adapter on a jittered schedule. Two items parked from Part 1 open the plan.

**Tech Stack:** Rust, DuckDB via `duckdb-rs`, `quick-xml` 0.41 (already in `Cargo.lock` via gpui — no new crate), `chrono`, `rand` 0.9 (already a `geode-demo-data` dependency), `scripts/mutation-check.sh`.

**Spec:** `docs/superpowers/specs/2026-09-12-geode-market-data-documents-design.md` — **§5, §6, §10 and §12 part 2 are this plan's whole brief**, with §4.5 "As built (Part 1)" binding for what already exists. §8 and §9 are Parts 3–4 (background only; the `Egress` trait and the channel adapter's echo are §5.2/§5.5 and ARE in scope, `Request::Upload` is not). **Also binding:** `docs/superpowers/specs/2026-09-12-geode-modules-roadmap.md` rulings 1, 3, 5 and 8; `docs/PHILOSOPHY.md` §6; CLAUDE.md's "Market-data documents Part 1" paragraph and the "Workspace invariants" list.

## Global Constraints

- CI runs on **macOS and Windows**: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo bench --workspace --no-run`, `cargo check -p geode-shell --features test-support --all-targets`. Run all five before every commit that touches Rust. No platform-specific code anywhere in this plan.
- **TDD**: write the failing test, run it, watch it fail for the right reason, then implement. Report RED honestly (a compile error is a legitimate RED for a new module; say so).
- **A mutation entry for every behaviour changed**, appended immediately after the last `run_mutation` entry in `scripts/mutation-check.sh` (before the `if [[ -n "$changed_ref" ]]` block), each naming the test expected to catch it as its 6th argument (`geode-core`, `geode-data`, `geode-documents`, `geode-demo-data` or `geode-app` as the 5th). **Commit before you mutate.** Confirm every anchor occurs exactly once (`grep -c -F '<anchor>' <file>` prints `1`). `zsh scripts/mutation-check.sh --anchors-only` must exit 0 before every commit — `runner.rs` and `service.rs` are heavily anchored; run it after every edit there and re-anchor what it reports to the same site with the same meaning, never by deleting an entry.
- **Run the harness DETACHED** (`nohup zsh scripts/mutation-check.sh --changed=main > /tmp/mut.log 2>&1 &`; wait with a bounded background `until` loop, never a foreground sleep). Verify `git status --porcelain` is clean afterwards.
- **Sockets:** nothing in this plan opens one. `ChannelAdapter` is in-process. The vendor Solace crate is NOT built here (roadmap ruling 5); the traits are its contract.
- **Layering:** `geode-documents` depends on `geode-core` only. `geode-demo-data` gains a dependency on `geode-core` only (it produces `DocumentRows`; it never writes XML). `geode-data` depends on `geode-documents`? **No** — the app registers kinds (§6.4); `geode-data` sees only the `DocumentKind` trait from core. `geode-app` depends on `geode-documents`. `geode-shell` never depends on `geode-data`.
- **No row objects; allocation is a cost** (PHILOSOPHY §6): the receiver thread parses into struct-of-arrays, the coalescer stores at most one pending document per key, and per-message work allocates the parsed columns and nothing per row beyond them.
- **Nothing blocks a producer:** every sink is `try_send` on a bounded channel with a counted refusal, exactly like `EventSink` (`crates/geode-data/src/service.rs`'s doc on `EventSink`) and `IngestSink`. No adapter thread may block on the service.
- **Health lanes** (CLAUDE.md, Phase 4b): connection state goes through `HealthTracker::report_discovery_and_emit`; parse/publish failures through `report_load_and_emit` keyed by batch. A clean discovery report must never clear a load-lane problem — that property already holds in the tracker; do not touch it.
- **Every diagnostic a reader emits fills `Diagnostic.path`** (`sources.<name>.<key>` for `SourceSpec`, `datasets.<ds>…` for schema).
- **Doc comments explain WHY, densely. A comment that contradicts the code is a defect.** The runner's module doc says "one thread owning the writer connection for every dataset, working a priority-ordered queue" — Task 8 adds a second queue and must rewrite that sentence. `distinct.rs`'s document-arm comment, spec §4.5 and CLAUDE.md all say the text filter "over-counts" — Task 1 makes that false and must rewrite all three.
- **Topic pattern syntax** (one place, tested): `>` as the final level matches one or more trailing levels; `*` matches exactly one level; anything else is a literal level; levels separated by `/`. This is Solace's syntax and what the real adapter will inherit.

## As-built vocabulary this plan builds on

```rust
// crates/geode-core/src/document.rs (Part 1)
pub const KEY_SEPARATOR: char = '\u{1f}';
pub fn join_key(parts: &[String]) -> String;  pub fn split_key(batch: &str) -> Vec<String>;
pub enum Column { F64(Vec<f64>), I64(Vec<i64>), Utf8(Vec<String>), Date(Vec<NaiveDate>) }   // ::len(), ::column_type()
pub enum Value { F64(f64), I64(i64), Utf8(String), Date(NaiveDate) }                         // ::column_type()
pub struct DocumentRows { pub key: Vec<String>, pub attributes: Vec<(String, Value)>, pub axes: Vec<(String, Column)>, pub values: Vec<(String, Column)> }
impl DocumentRows { pub fn rows(&self) -> usize; pub fn validate(&self, ds: &DatasetSpec) -> Result<(), String> }  // refuses zero rows

// crates/geode-core/src/schema (Part 1)
pub enum Family { Measures, Document }
pub struct DatasetSpec { pub name, pub columns: Vec<ColumnSpec>, pub family: Family, pub key: Vec<String>, pub axes: Vec<String> }
impl DatasetSpec { pub fn is_document(&self) -> bool; pub fn document_columns(&self) -> Vec<&ColumnSpec>; pub fn column(&self, &str) -> Option<&ColumnSpec> }
pub struct ColumnSpec { pub name: String, pub ty: ColumnType, pub role: ColumnRole, pub textual: bool, pub categorical: bool, .. }
pub enum ColumnType { Utf8, F64, I64, Date, Timestamp, Bool }   // document family allows only the first four
pub enum ColumnRole { Key, Dimension { grain: Option<Grain> }, Measure {..}, Attribute { grain: Option<Grain> }, Axis, Value }

// crates/geode-core/src/source_config.rs (as it stands)
pub struct SourceSpec { pub name, pub dataset, pub paths: Vec<String>, pub readiness: Readiness, pub priority: Priority, pub poll_interval: Duration, pub pending_timeout: Duration, pub batch_pattern: Option<String> }
impl SourceSpec { pub fn from_doc(doc: &MergedDoc, schema: &SchemaSpec) -> (Vec<SourceSpec>, Vec<Diagnostic>) }
pub fn parse_duration(s: &str) -> Option<Duration>;   // "30s" | "10m" | "2h"
fn diag(severity, name, key: Option<&str>, m) -> Diagnostic   // path = "sources.{name}[.{key}]"
pub enum Priority { LatestRisk, LatestOther, Backfill }

// crates/geode-core/src/health.rs
pub enum Health { Ok, Pending, PendingTooLong, Degraded { reason: String }, Failed { reason: String } }

// crates/geode-data/src/store/document.rs (Part 1)
pub struct DocumentPublishRequest<'a> { pub dataset: &'a DatasetSpec, pub source: &'a str, pub rows: &'a DocumentRows, pub source_time: DateTime<Utc>, pub received_at: DateTime<Utc>, pub bytes: u64 }
pub struct DocumentPublished { pub batch: String, pub gen_id: i64, pub rows: usize, pub outcome: PublishOutcome }
pub fn publish_document(store: &Store, req: &DocumentPublishRequest) -> Result<DocumentPublished, StoreError>

// crates/geode-data/src/ingest/runner.rs (as it stands)
pub enum IngestEvent { Published { source, dataset, batch, gen_id, books: Vec<Option<String>>, rows: usize, health: Health }, Failed { source, dataset, batch, reason: String }, PlanComplete }
pub type IngestSink = Arc<dyn Fn(IngestEvent) -> bool + Send + Sync>;
struct Queue { items: Vec<WorkItem>, shutdown: bool, in_flight: Option<(PathBuf, u64, DateTime<Utc>)> }
pub struct IngestHandle { queue: Arc<(Mutex<Queue>, Condvar)>, thread: Mutex<Option<JoinHandle<()>>> }
impl IngestRunner { pub fn spawn(store: Store, schema: SchemaSpec, sink: IngestSink) -> IngestHandle; pub fn spawn_channel(store, schema) -> (IngestHandle, Receiver<IngestEvent>) }
impl IngestHandle { pub fn submit(&self, plan: WorkPlan) -> usize; pub fn shutdown(&self) }
fn run(store: Store, schema: SchemaSpec, queue: Arc<(Mutex<Queue>, Condvar)>, sink: IngestSink, load: LoadFn)   // the loop; pops q.items[0]; load under catch_unwind + geode_core::panic::contained; emits Published/Failed

// crates/geode-data/src/ingest/scheduler.rs
impl Scheduler { pub fn spawn(sources: Vec<SourceSpec>, conn: duckdb::Connection, ingest: Arc<IngestHandle>, sink: SchedulerSink) -> Scheduler; pub fn shutdown(&self) }
pub enum SchedulerEvent { Polled { source, ready, next_in }, Health { source, worst, detail } }

// crates/geode-data/src/service.rs
pub struct DataServiceConfig { pub db_path: PathBuf, pub schema: SchemaSpec, pub views: Vec<ViewSpec>, pub dimensions: DerivedDimensions, pub query_workers: usize, pub sources: Vec<SourceSpec> }
pub struct DataService { config, diagnostics: Vec<Diagnostic>, pool: QueryPool, scheduler: Scheduler, ingest: Arc<IngestHandle>, conn: duckdb::Connection }
impl DataService { pub fn open(config, sink: EventSink) -> Result<DataService, StoreError>; pub fn open_channel(config) -> Result<(DataService, Receiver<DataEvent>), StoreError>; pub fn shutdown(&self) }
struct HealthTracker;  // Arc-shared by the ingest sink and the scheduler sink
impl HealthTracker { fn report_discovery_and_emit(&self, source: &str, health: Health, detail: String, emit: impl FnOnce(Option<(Health, String)>) -> bool) -> bool;
                     fn report_load_and_emit(&self, source: &str, batch: &str, health: Health, detail: String, emit: impl FnOnce(Option<(Health, String)>) -> bool) -> bool }
fn log_health_event(source: &str, worst: &Health, detail: &str);
pub enum DataEvent { Query(..), Distinct(..), Catalog(..), Published { dataset, batch, gen_id, books }, Health { source, worst, detail }, Polled {..}, Diagnostics(Vec<Diagnostic>) }

// crates/geode-data/src/query/distinct.rs (Part 1 final fix wave)
fn document_select(..)   // the document arm; applies dimension selections only — Task 1 fixes this
// crates/geode-data/src/store/ddl.rs
pub mod tests_support { pub fn cvi_dataset() -> DatasetSpec }   // #[cfg(test)] pub(crate), the one CVI fixture to dedupe onto

// crates/geode-app/src/demo.rs
pub fn demo_dir(rows) -> PathBuf; pub fn ensure_emitted(dir, rows) -> io::Result<PathBuf>; pub fn layer(source_dir: &Path) -> Vec<LayerDoc>   // builds the `sources` doc text inline
// crates/geode-app/src/bridge.rs
pub fn data_setup(config: &Config, db_path: PathBuf) -> Option<DataSetup>   // builds DataServiceConfig { .., sources }
pub fn start(setup: DataSetup, ..) -> Bridge   // DataService::spawn(config, sink)
```

---

### Task 1: The document arm of `compile_distinct` applies the text and expression filters (parked from Part 1)

**Files:**
- Modify: `crates/geode-data/src/query/distinct.rs` (`document_select`)
- Modify: `crates/geode-data/src/query/scope_sql.rs` (expose the text-block pieces if not already `pub(crate)`: `like_pattern`, the cache's `matches`, and the expression lowering for a single relation)
- Modify: spec §4.5, CLAUDE.md paragraph (the three disclosures)
- Test: `crates/geode-data/src/query/distinct.rs` tests
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Consumes: `Scope { text, expression, dimensions }`, `DatasetSpec::textual_columns()`, `scope_sql`'s text-filter primitives (read `compile_scope_cached`'s text block: `like_pattern`, the ENUM-dictionary rewrite via the cache, and the `false`-if-no-textual-columns rule at the end of the block).
- Produces: `document_select` whose `where` clause is the AND of: the applicable dimension selections (as today), the text filter lowered grain-free (an OR of `"<col>" ILIKE ?` over `ds.textual_columns()` — through the cache's dictionary rewrite for a categorical column when the cache has one, exactly as the measure path does — and `false` when the text is set and the dataset has no textual column), and the expression lowered over the same relation. No `route`, `membership` or `Era::relation` step: a document dataset is one table.

- [ ] **Step 1: Write the failing tests**

In `distinct.rs`'s test module, beside the Part 1 document tests (they share the `document_fixture()`-style helper that publishes two CVI documents; reuse it):

```rust
    #[test]
    fn a_text_filter_narrows_a_document_datasets_contribution() {
        // cvi_params.underlying_ref is textual. "spx" must keep SPX.Z and drop NDX.Z.
        let (_d, store, schema, cache) = document_fixture(); // whatever the existing helper returns
        let scope = Scope { text: Some("spx".into()), ..Scope::default() };
        let params = distinct_params("underlying_ref", scope, AsOf::Live);
        let compiled = compile_distinct_with_cache(store.writer(), &schema, &DerivedDimensions::default(), &params, &cache).unwrap();
        let values = run_distinct(&store, &compiled);
        assert_eq!(values, vec![("SPX.Z".to_string(), 6)]);
    }

    #[test]
    fn a_text_filter_on_a_document_dataset_with_no_textual_column_contributes_nothing() {
        // Same rule as the measure path's `false`-if-empty: a needle over a
        // dataset that cannot be searched matches nothing, never everything.
        let (_d, store, mut schema, cache) = document_fixture();
        for c in &mut schema.datasets.iter_mut().find(|d| d.name == "cvi_params").unwrap().columns { c.textual = false; }
        let scope = Scope { text: Some("spx".into()), ..Scope::default() };
        let params = distinct_params("underlying_ref", scope, AsOf::Live);
        let compiled = compile_distinct_with_cache(store.writer(), &schema, &DerivedDimensions::default(), &params, &cache).unwrap();
        assert!(run_distinct(&store, &compiled).is_empty());
    }

    #[test]
    fn an_expression_filter_narrows_a_document_datasets_contribution() {
        let (_d, store, schema, cache) = document_fixture();
        let scope = Scope { expression: Some(geode_core::scope::parse_expr("spot_ref > 7000").unwrap()), ..Scope::default() };
        // Give the fixture's NDX.Z document a spot_ref below 7000 if it does not already differ; adjust the helper if needed.
        let params = distinct_params("underlying_ref", scope, AsOf::Live);
        let compiled = compile_distinct_with_cache(store.writer(), &schema, &DerivedDimensions::default(), &params, &cache).unwrap();
        let values = run_distinct(&store, &compiled);
        assert_eq!(values.iter().map(|(v, _)| v.as_str()).collect::<Vec<_>>(), vec!["SPX.Z"]);
    }
```

Use the exact names of the existing helpers in that module (`distinct_params`, `run_distinct`, the fixture) — read them first; the assertions are the requirement. If `parse_expr` has a different name in `geode_core::scope::expr`, use that.

- [ ] **Step 2: Run to verify failure** — `cargo test -p geode-data distinct` → the three new tests FAIL (the first returns both keys, the second returns both, the third returns both).

- [ ] **Step 3: Implement**

In `document_select`, after the dimension-selection clauses, build two more clauses:

```rust
    // Text filter, grain-free (Part 2 Task 1; Part 1 shipped this arm with
    // dimension selections only and disclosed it). A document dataset is
    // one table, so every grain-dependent step of the measure path's text
    // block — `route`, `membership`, `Era::relation` — is a no-op here;
    // what remains is the OR over the dataset's textual columns, through
    // the ENUM dictionary rewrite where the cache has one, and the same
    // `false`-if-no-textual-column rule: a needle over a dataset that
    // cannot be searched must narrow to nothing, never widen to
    // everything (`scope_sql.rs`'s own comment on that rule).
    if let Some(text) = scope.text.as_deref().filter(|t| !t.trim().is_empty()) {
        let mut terms: Vec<String> = Vec::new();
        for col in ds.textual_columns() {
            match cache.matches(&ds.name, &col.name, text) {   // the dictionary rewrite the measure path uses; adapt to its real signature
                Some(values) if values.is_empty() => {}          // categorical, nothing matches: contributes no term
                Some(values) => terms.push(in_list(&col.name, &values, &mut params)),
                None => { terms.push(format!("\"{}\" ILIKE ?", col.name)); params.push(like_pattern(text).into()); }
            }
        }
        clauses.push(if terms.is_empty() { "false".to_string() } else { format!("({})", terms.join(" or ")) });
    }
    if let Some(expr) = &scope.expression {
        // Same lowering the measure path uses for a single relation; the
        // document family has no derived-dimension translation (no grain
        // to route a derived column through), so an expression naming one
        // is refused at compile time rather than silently widened.
        let (sql, p) = scope_sql::lower_expression_for(expr, ds, dims)?;   // adapt to the real helper name; extract one if the measure path inlines it
        clauses.push(sql);
        params.extend(p);
    }
```

Read `compile_scope_cached`'s text block and reuse its helpers by name; extract a `pub(crate)` helper from `scope_sql.rs` if the text-term building is inlined there, without changing the measure path's SQL (its tests and mutation entries pin it — run `--anchors-only` after the edit).

- [ ] **Step 4: Run to verify pass** — `cargo test -p geode-data distinct scope_sql` → PASS; `cargo test -p geode-data` → PASS.

- [ ] **Step 5: Rewrite the three disclosures**

`distinct.rs`'s document-arm comment, spec §4.5's bullet on the picker, and CLAUDE.md's paragraph sentence all currently say the document arm applies dimension selections only and can over-count under a text filter. Replace each with: the text and expression filters are lowered grain-free over the document table, with the measure path's `false`-if-no-textual-column rule.

- [ ] **Step 6: Mutation entries and commit**

```zsh
run_mutation "distinct/document: a text filter narrows the document contribution" \
  crates/geode-data/src/query/distinct.rs \
  '        clauses.push(if terms.is_empty() { "false".to_string() } else { format!("({})", terms.join(" or ")) });' \
  '        let _ = &terms;' \
  geode-data a_text_filter_narrows_a_document_datasets_contribution

run_mutation "distinct/document: no textual column means nothing, not everything" \
  crates/geode-data/src/query/distinct.rs \
  '        clauses.push(if terms.is_empty() { "false".to_string() } else { format!("({})", terms.join(" or ")) });' \
  '        clauses.push(if terms.is_empty() { "true".to_string() } else { format!("({})", terms.join(" or ")) });' \
  geode-data a_text_filter_on_a_document_dataset_with_no_textual_column_contributes_nothing
```

(Two entries on one anchor line is fine: each mutates it differently and names a different test; the anchor still occurs once.) Add a third for the expression clause (`clauses.push(sql);` → `let _ = sql;`) naming the expression test. Commit: `git commit -m "distinct: the document arm lowers text and expression filters grain-free (Part 1 parked item)"`.

---

### Task 2: One CVI fixture for `geode-data`'s tests (parked from Part 1)

**Files:**
- Modify: `crates/geode-data/src/store/ddl.rs` (`tests_support`: add `cvi_doc`, `ts`, `d`, `publish_cvi` helpers beside `cvi_dataset`)
- Modify: `crates/geode-data/src/store/document.rs`, `crates/geode-data/src/query/document.rs`, `crates/geode-data/src/query/catalog.rs`, `crates/geode-data/src/query/distinct.rs`, `crates/geode-data/src/service.rs`, `crates/geode-data/src/handle.rs` test modules (delete their private copies; import from `crate::store::ddl::tests_support`)

**Interfaces:**
- Produces (all `#[cfg(test)] pub(crate)` in `ddl::tests_support`):

```rust
pub(crate) fn cvi_dataset() -> DatasetSpec;                                   // exists
pub(crate) fn cvi_doc(key: &str, params: [f64; 6]) -> DocumentRows;           // two terms × three nodes, term-major, anchor 2026-09-12, spot 7650
pub(crate) fn ts(s: &str) -> DateTime<Utc>;                                    // RFC 3339
pub(crate) fn d(s: &str) -> NaiveDate;                                         // %Y-%m-%d
pub(crate) fn publish_cvi(store: &Store, ds: &DatasetSpec, key: &str, params: [f64; 6], at: &str) -> DocumentPublished;  // source "cvi", received_at = source_time, bytes 1234
```

- [ ] **Step 1: Move, don't rewrite.** Take the copy in `store/document.rs`'s tests as the canonical text for `cvi_doc`/`ts`/`d`; add `publish_cvi` wrapping `publish_document`. Delete every other copy and fix imports. The term-major fixture shape is load-bearing (Part 1's axis-order mutation entry bites only because of it): assert it once in `tests_support` with a test `the_cvi_fixture_is_term_major` (`cvi_doc("X", [1.,2.,3.,4.,5.,6.]).axes[0].1` is `[09-18, 09-18, 09-18, 10-16, 10-16, 10-16]`).
- [ ] **Step 2: Verify** — `cargo test -p geode-data` → same count as before plus one, all PASS; `zsh scripts/mutation-check.sh --anchors-only` → exit 0 (test modules are not anchored, but confirm); `cargo clippy --workspace --all-targets -- -D warnings`.
- [ ] **Step 3: Commit** — `git commit -m "geode-data tests: one CVI fixture in ddl::tests_support (Part 1 parked item)"`.

---

### Task 3: `DocumentKind` — the parser/writer contract, and the registry

**Files:**
- Modify: `crates/geode-core/src/document.rs` (append the trait and its types)
- Create: `crates/geode-data/src/documents.rs` (`DocumentRegistry`)
- Modify: `crates/geode-data/src/lib.rs` (`pub mod documents;`)
- Test: both files' test modules

**Interfaces:**
- Produces, in `geode_core::document`:

```rust
/// What a parser reports beside the rows: element paths it did not
/// recognise. The receiver logs each once per source (spec §6.3).
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedDocument { pub rows: DocumentRows, pub unknown_paths: Vec<String> }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError { pub message: String }
impl std::fmt::Display for ParseError { .. }   // "{message}"
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteError { pub message: String }
impl std::fmt::Display for WriteError { .. }

pub trait DocumentKind: Send + Sync {
    fn name(&self) -> &'static str;
    /// The columns this kind produces — key parts, axes, values, attributes
    /// — with their types, checked against the dataset it feeds when the
    /// source opens (spec §6.4).
    fn columns(&self) -> &[(&'static str, ColumnType)];
    fn parse(&self, bytes: &[u8]) -> Result<ParsedDocument, ParseError>;
    fn write(&self, rows: &DocumentRows) -> Result<Vec<u8>, WriteError>;
}

/// The load-time check in spec §6.4: every column the kind produces is
/// declared on the dataset with the same type, and every column the
/// dataset declares (key, axes, values, document-level attributes) is one
/// the kind produces. Both directions, so a document can be staged in
/// `document_columns()` order by construction.
pub fn check_kind_against(kind: &dyn DocumentKind, ds: &DatasetSpec) -> Result<(), String>;
```

and in `geode_data::documents`:

```rust
#[derive(Default, Clone)]
pub struct DocumentRegistry { kinds: HashMap<String, Arc<dyn DocumentKind>> }
impl DocumentRegistry {
    pub fn register(&mut self, kind: Arc<dyn DocumentKind>);   // keyed by kind.name(); a second registration of a name replaces and warns at geode::ingest
    pub fn get(&self, name: &str) -> Option<Arc<dyn DocumentKind>>;
    pub fn names(&self) -> Vec<String>;
}
```

- [ ] **Step 1: Write the failing tests**

In `geode-core` `document::tests`, a `FakeKind` implementing the trait with `columns()` = the CVI fixture's six `(name, type)` pairs, `parse` returning `sample()` for any bytes, `write` returning `b"fake"`:

```rust
    #[test]
    fn check_kind_against_accepts_a_matching_dataset_and_names_each_mismatch() {
        let ds = cvi();
        assert_eq!(check_kind_against(&FakeKind::default(), &ds), Ok(()));
        // The kind produces a column the dataset lacks.
        let extra = FakeKind { columns: with_extra(("vol", ColumnType::F64)) };
        assert!(check_kind_against(&extra, &ds).unwrap_err().contains("kind produces 'vol', which dataset 'cvi_params' does not declare"));
        // The dataset declares a column the kind does not produce.
        let missing = FakeKind { columns: without("spot_ref") };
        assert!(check_kind_against(&missing, &ds).unwrap_err().contains("dataset 'cvi_params' declares 'spot_ref', which kind 'fake' does not produce"));
        // Same name, different type.
        let wrong = FakeKind { columns: retyped("node", ColumnType::I64) };
        assert!(check_kind_against(&wrong, &ds).unwrap_err().contains("'node' is i64 in the kind, f64 in the dataset"));
        // A measure dataset is refused outright.
        let mut m = ds.clone(); m.family = Family::Measures;
        assert!(check_kind_against(&FakeKind::default(), &m).unwrap_err().contains("not a document dataset"));
    }
```

In `geode-data` `documents::tests`: register two kinds, `get` by name, `names()` sorted, re-registering a name replaces (assert the second `Arc` is returned).

- [ ] **Step 2: Run to verify failure** — compile errors (trait, fn, module missing).

- [ ] **Step 3: Implement** — the trait and types as above; `check_kind_against` compares `kind.columns()` against `ds.document_columns()` by name in both directions then by type, building the messages above. `DocumentRegistry` as above with `tracing::warn!(target: "geode::ingest", ..)` on replacement.

- [ ] **Step 4: Run to verify pass** — `cargo test -p geode-core document::` and `cargo test -p geode-data documents::` → PASS.

- [ ] **Step 5: Mutation entries and commit**

```zsh
run_mutation "document kind: a column the dataset lacks is a mismatch" \
  crates/geode-core/src/document.rs \
  '<the line pushing/returning the "kind produces" error — copy verbatim>' \
  '<the same line made unreachable, e.g. `if false {`>' \
  geode-core check_kind_against_accepts_a_matching_dataset_and_names_each_mismatch
```

Add the mirror entry for the "dataset declares" direction and one for the type comparison. Commit: `git commit -m "core: DocumentKind trait, check_kind_against; data: DocumentRegistry"`.

---

### Task 4: The `geode-documents` crate with the CVI kind

**Files:**
- Create: `crates/geode-documents/Cargo.toml`, `crates/geode-documents/src/lib.rs`, `crates/geode-documents/src/cvi.rs`, `crates/geode-documents/benches/cvi.rs`
- Modify: `Cargo.toml` (workspace `members`)
- Test: `crates/geode-documents/src/cvi.rs` tests (unit + a `proptest` round trip)

**Interfaces:**
- Produces: `geode_documents::cvi::CviKind` (unit struct implementing `DocumentKind` with `name() == "cvi_params"`), `pub const NAME: &str = "cvi_params"`; the crate re-exports `pub use cvi::CviKind;` and `pub fn builtin_kinds() -> Vec<Arc<dyn DocumentKind>>` (today: `[CviKind]`).
- `CviKind::columns()` = `[("underlying_ref", Utf8), ("term", Date), ("node", F64), ("param", F64), ("anchor_date", Date), ("spot_ref", F64)]` — the same six the demo `cvi_params` declares, in `document_columns()` order.

- [ ] **Step 1: Crate scaffold**

`Cargo.toml`:

```toml
[package]
name = "geode-documents"
version.workspace = true
edition.workspace = true
publish.workspace = true

[lib]
bench = false

[dependencies]
geode-core.workspace = true
chrono = "0.4.42"
# XML for the desk's market-data documents (spec §6). Already in the lock
# via gpui, so this pins the version cargo already resolves and adds no crate.
quick-xml = "0.41"

[dev-dependencies]
criterion = "0.8.2"
proptest = "1"

[[bench]]
name = "cvi"
harness = false
```

Add `"crates/geode-documents"` to the workspace `members` in the root `Cargo.toml`. `lib.rs`:

```rust
//! Document kinds (market-data spec §6): one typed parser and writer per
//! kind, pure — no I/O, no gpui — depended on by the app (which registers
//! them into the data service) and by nothing in `geode-data` itself: the
//! service sees only `geode_core::document::DocumentKind`. Hand-written
//! over `quick-xml` now; regenerated from the desk's XSDs behind the same
//! two functions later (roadmap ruling 8).
pub mod cvi;
pub use cvi::CviKind;
use geode_core::document::DocumentKind;
use std::sync::Arc;
pub fn builtin_kinds() -> Vec<Arc<dyn DocumentKind>> { vec![Arc::new(CviKind)] }
```

- [ ] **Step 2: Write the failing tests** (in `cvi.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::document::{Column, DocumentKind, DocumentRows, Value};
    use chrono::NaiveDate;
    fn d(s: &str) -> NaiveDate { NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap() }

    const DOC: &str = r#"<?xml version="1.0"?>
<marketData>
  <underlying>SPX.Z</underlying>
  <cviParams>
    <anchorDate>2026-09-12</anchorDate>
    <spotRef>7650</spotRef>
    <nodes><node>-20.0</node><node>-1</node><node>3.5</node></nodes>
    <slices>
      <slice><term>2026-09-18</term><param>-0.34</param><param>0.1</param><param>1.3</param></slice>
      <slice><term>2026-10-16</term><param>-0.3</param><param>0.12</param><param>1.25</param></slice>
    </slices>
  </cviParams>
</marketData>"#;

    fn expected() -> DocumentRows {
        DocumentRows {
            key: vec!["SPX.Z".into()],
            attributes: vec![("anchor_date".into(), Value::Date(d("2026-09-12"))), ("spot_ref".into(), Value::F64(7650.0))],
            axes: vec![
                ("term".into(), Column::Date(vec![d("2026-09-18"); 3].into_iter().chain(vec![d("2026-10-16"); 3]).collect())),
                ("node".into(), Column::F64(vec![-20.0, -1.0, 3.5, -20.0, -1.0, 3.5])),
            ],
            values: vec![("param".into(), Column::F64(vec![-0.34, 0.1, 1.3, -0.3, 0.12, 1.25]))],
        }
    }

    #[test]
    fn parses_the_sketch_document_term_major() {
        let parsed = CviKind.parse(DOC.as_bytes()).unwrap();
        assert_eq!(parsed.rows, expected());
        assert!(parsed.unknown_paths.is_empty());
    }

    #[test]
    fn a_ragged_slice_fails_with_both_counts() {
        let doc = DOC.replace("<param>1.25</param>", "");
        let err = CviKind.parse(doc.as_bytes()).unwrap_err();
        assert!(err.message.contains("slice 2026-10-16 has 2 params, nodes has 3"), "{}", err.message);
    }

    #[test]
    fn an_unknown_element_is_skipped_and_reported_by_path() {
        let doc = DOC.replace("<spotRef>7650</spotRef>", "<spotRef>7650</spotRef><vendorNote>x</vendorNote>");
        let parsed = CviKind.parse(doc.as_bytes()).unwrap();
        assert_eq!(parsed.rows, expected());
        assert_eq!(parsed.unknown_paths, vec!["marketData/cviParams/vendorNote".to_string()]);
    }

    #[test]
    fn each_missing_required_element_fails() {
        for (needle, what) in [
            ("<underlying>SPX.Z</underlying>", "underlying"),
            ("<anchorDate>2026-09-12</anchorDate>", "anchorDate"),
            ("<nodes><node>-20.0</node><node>-1</node><node>3.5</node></nodes>", "nodes"),
            ("<term>2026-09-18</term>", "term"),
        ] {
            let doc = DOC.replace(needle, "");
            let err = CviKind.parse(doc.as_bytes()).unwrap_err();
            assert!(err.message.contains(what), "{what}: {}", err.message);
        }
        let doc = DOC.replace("<slices>", "<slices></slices><old>").replace("</slices>", "</old>");
        assert!(CviKind.parse(doc.as_bytes()).is_err(), "no slices is an error");
    }

    #[test]
    fn a_non_numeric_param_or_bad_date_fails_naming_the_value() {
        let err = CviKind.parse(DOC.replace("<param>0.1</param>", "<param>abc</param>").as_bytes()).unwrap_err();
        assert!(err.message.contains("param 'abc'"), "{}", err.message);
        let err = CviKind.parse(DOC.replace("2026-09-18", "18/09/2026").as_bytes()).unwrap_err();
        assert!(err.message.contains("term '18/09/2026'"), "{}", err.message);
    }

    #[test]
    fn write_then_parse_round_trips_the_expected_rows() {
        let bytes = CviKind.write(&expected()).unwrap();
        let parsed = CviKind.parse(&bytes).unwrap();
        assert_eq!(parsed.rows, expected());
    }

    #[test]
    fn write_refuses_rows_that_are_not_a_full_grid() {
        let mut rows = expected();
        rows.values[0].1 = Column::F64(vec![1.0; 5]);
        assert!(CviKind.write(&rows).unwrap_err().message.contains("6 rows"));
        let mut rows = expected();
        rows.key = vec!["SPX.Z".into(), "NDX.Z".into()];
        assert!(CviKind.write(&rows).unwrap_err().message.contains("one key part"));
    }

    proptest::proptest! {
        #[test]
        fn generated_grids_round_trip(
            terms in proptest::collection::vec(0u32..2000, 1..8),
            nodes in proptest::collection::vec(-30.0f64..30.0, 1..12),
            seed in 0u64..1000,
        ) {
            // Distinct, sorted terms as dates off 2026-01-01; params from a cheap LCG so the
            // values are arbitrary but finite with a short decimal form.
            let mut terms: Vec<u32> = terms; terms.sort_unstable(); terms.dedup();
            let base = d("2026-01-01");
            let term_dates: Vec<NaiveDate> = terms.iter().map(|t| base + chrono::Days::new(*t as u64)).collect();
            let mut x = seed;
            let mut params = Vec::new();
            let mut term_col = Vec::new();
            let mut node_col = Vec::new();
            for t in &term_dates { for n in &nodes {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                params.push(((x >> 33) as f64 / (1u64 << 31) as f64 * 4.0 - 2.0 * 1000.0).round() / 1000.0);
                term_col.push(*t); node_col.push(*n);
            }}
            let rows = DocumentRows {
                key: vec!["SPX.Z".into()],
                attributes: vec![("anchor_date".into(), Value::Date(base)), ("spot_ref".into(), Value::F64(7650.0))],
                axes: vec![("term".into(), Column::Date(term_col)), ("node".into(), Column::F64(nodes.clone()).repeat_for(term_dates.len()))],
                values: vec![("param".into(), Column::F64(params))],
            };
            let bytes = CviKind.write(&rows).unwrap();
            let parsed = CviKind.parse(&bytes).unwrap();
            proptest::prop_assert_eq!(parsed.rows, rows);
        }
    }
}
```

(`repeat_for` does not exist — build the node column with a plain loop instead; the property is what matters. Round-trip exactness for f64 requires the writer to print with `{}` — Rust's shortest-round-trip formatting — and the parser to use `str::parse::<f64>`; that pair is exact.)

- [ ] **Step 3: Run to verify failure** — compile errors (module missing).

- [ ] **Step 4: Implement `cvi.rs`**

Parse with `quick_xml::Reader` events, tracking a path stack of element names (`Vec<String>`), so an unknown element's path is `stack.join("/")` and its whole subtree is skipped (`reader.read_to_end`). Required elements: `underlying`, `cviParams/anchorDate`, `cviParams/nodes` with ≥1 `node`, `cviParams/slices` with ≥1 `slice`, each slice with `term` and exactly `nodes.len()` `param`s. `spotRef` optional? No — the CVI dataset declares `spot_ref` as an attribute and `DocumentRows::validate` requires it, so it is required here too (message names `spotRef`). Numbers via `str::parse::<f64>()` (error: `param '{text}' is not a number`), dates via `NaiveDate::parse_from_str(s, "%Y-%m-%d")` (error: `term '{text}' is not a date (YYYY-MM-DD)`). Whitespace-only text between elements is ignored. Produce term-major rows.

Write with `quick_xml::Writer`, indented two spaces, elements in the sketch's order, numbers with `{}` formatting, dates `%Y-%m-%d`. Refuse: `rows.key.len() != 1` ("CVI has one key part"), axes not `[term, node]`, values not `[param]`, a value/axis length that is not `terms × nodes` for the distinct terms and the distinct nodes in first-appearance order ("expected {t}×{n} = {tn} rows, got {r}"), a term whose node list differs from the first term's (the grid must be full).

- [ ] **Step 5: Bench** — `benches/cvi.rs`: criterion over `parse` and `write` at 20 terms × 30 nodes and 200 × 300 (built from `expected()`'s shape scaled). `cargo bench -p geode-documents --no-run` compiles.

- [ ] **Step 6: Run to verify pass** — `cargo test -p geode-documents` (proptest default 256 cases) → PASS; clippy clean; `cargo bench --workspace --no-run` clean.

- [ ] **Step 7: Mutation entries and commit**

```zsh
run_mutation "cvi: a ragged slice is refused" \
  crates/geode-documents/src/cvi.rs \
  '<the `if params.len() != nodes.len()` line — copy verbatim>' \
  '<same line with `if false`>' \
  geode-documents a_ragged_slice_fails_with_both_counts

run_mutation "cvi: an unknown element is reported, not swallowed" \
  crates/geode-documents/src/cvi.rs \
  '<the `unknown_paths.push(...)` line>' \
  '<the same line replaced by `let _ = ();`>' \
  geode-documents an_unknown_element_is_skipped_and_reported_by_path

run_mutation "cvi: write emits params in node order" \
  crates/geode-documents/src/cvi.rs \
  '<the loop that writes one <param> per node, its first line>' \
  '<the same loop iterating nodes in reverse (`.iter().rev()`)>' \
  geode-documents write_then_parse_round_trips_the_expected_rows
```

Commit: `git commit -m "geode-documents: the CVI kind — quick-xml parser and writer, round-trip property, bench"`.

---

### Task 5: `sources.toml` grows an adapter

**Files:**
- Modify: `crates/geode-core/src/source_config.rs`
- Test: its test module
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Produces:

```rust
pub const CSV_DIR_ADAPTER: &str = "csv_dir";
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceTime { Receive, Document(String) }   // "receive" | "document:<field>"
pub struct SourceSpec {
    ..existing fields..,
    pub adapter: String,            // default CSV_DIR_ADAPTER
    pub document: Option<String>,   // required when adapter != csv_dir
    pub topics: Vec<String>,        // required non-empty when adapter != csv_dir
    pub coalesce: Duration,         // default 500ms; "0" = every message
    pub source_time: SourceTime,    // default Receive
}
impl SourceSpec { pub fn is_subscribed(&self) -> bool { self.adapter != CSV_DIR_ADAPTER } }
pub fn parse_duration(s: &str) -> Option<Duration>;   // now also "500ms"; "0" alone is Duration::ZERO
```

- [ ] **Step 1: Write the failing tests** (in `source_config::tests`, using the module's existing `doc(text)`/schema helpers; the schema must declare `cvi_params` as a document dataset — add the CVI TOML to the fixture)

```rust
    #[test]
    fn parse_duration_accepts_milliseconds_and_a_bare_zero() {
        assert_eq!(parse_duration("500ms"), Some(Duration::from_millis(500)));
        assert_eq!(parse_duration("0"), Some(Duration::ZERO));
        assert_eq!(parse_duration("2s"), Some(Duration::from_secs(2)));
        assert_eq!(parse_duration("5"), None, "a bare non-zero number has no unit");
        assert_eq!(parse_duration("ms"), None);
    }

    #[test]
    fn a_subscribed_source_parses_its_adapter_fields() {
        let (sources, diags) = from(r#"
[cvi]
adapter = "demo_bus"
dataset = "cvi_params"
document = "cvi_params"
topics = ["marketdata/cvi/>"]
coalesce = "250ms"
source_time = "receive"
priority = "latest_other"
"#);
        assert!(diags.is_empty(), "{diags:?}");
        let s = &sources[0];
        assert!(s.is_subscribed());
        assert_eq!((s.adapter.as_str(), s.document.as_deref()), ("demo_bus", Some("cvi_params")));
        assert_eq!(s.topics, vec!["marketdata/cvi/>".to_string()]);
        assert_eq!(s.coalesce, Duration::from_millis(250));
        assert_eq!(s.source_time, SourceTime::Receive);
        assert!(s.paths.is_empty());
    }

    #[test]
    fn a_directory_source_is_unchanged_and_defaults_its_adapter() {
        let (sources, diags) = from(r#"
[demo]
dataset = "risk_snapshot"
paths = ["/tmp/*.csv"]
"#);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(sources[0].adapter, CSV_DIR_ADAPTER);
        assert!(!sources[0].is_subscribed());
        assert_eq!(sources[0].coalesce, Duration::from_millis(500), "the default, unused by a directory source");
    }

    #[test]
    fn a_subscribed_source_needs_topics_and_a_document() {
        let (sources, diags) = from("[cvi]\nadapter = \"demo_bus\"\ndataset = \"cvi_params\"\ndocument = \"cvi_params\"\n");
        assert!(sources.is_empty());
        let d = diags.iter().find(|d| d.path.as_deref() == Some("sources.cvi.topics")).unwrap();
        assert_eq!(d.severity, Severity::Error);
        let (sources, diags) = from("[cvi]\nadapter = \"demo_bus\"\ndataset = \"cvi_params\"\ntopics = [\"a/>\"]\n");
        assert!(sources.is_empty());
        assert!(diags.iter().any(|d| d.path.as_deref() == Some("sources.cvi.document") && d.severity == Severity::Error));
    }

    #[test]
    fn directory_keys_on_a_subscribed_source_warn_and_a_directory_source_still_needs_paths() {
        let (sources, diags) = from("[cvi]\nadapter = \"demo_bus\"\ndataset = \"cvi_params\"\ndocument = \"cvi_params\"\ntopics = [\"a/>\"]\npaths = [\"/x\"]\npoll_interval = \"1s\"\n");
        assert_eq!(sources.len(), 1);
        for key in ["paths", "poll_interval"] {
            assert!(diags.iter().any(|d| d.path.as_deref() == Some(&format!("sources.cvi.{key}")) && d.severity == Severity::Warning), "{key}");
        }
        let (sources, diags) = from("[demo]\ndataset = \"risk_snapshot\"\n");
        assert!(sources.is_empty());
        assert!(diags.iter().any(|d| d.path.as_deref() == Some("sources.demo.paths")));
    }

    #[test]
    fn source_time_document_names_its_field_and_a_bad_value_warns_to_receive() {
        let (sources, _) = from("[cvi]\nadapter = \"demo_bus\"\ndataset = \"cvi_params\"\ndocument = \"cvi_params\"\ntopics = [\"a/>\"]\nsource_time = \"document:anchor_date\"\n");
        assert_eq!(sources[0].source_time, SourceTime::Document("anchor_date".into()));
        let (sources, diags) = from("[cvi]\nadapter = \"demo_bus\"\ndataset = \"cvi_params\"\ndocument = \"cvi_params\"\ntopics = [\"a/>\"]\nsource_time = \"yesterday\"\n");
        assert_eq!(sources[0].source_time, SourceTime::Receive);
        assert!(diags.iter().any(|d| d.path.as_deref() == Some("sources.cvi.source_time") && d.severity == Severity::Warning));
    }

    #[test]
    fn a_subscribed_source_on_a_measure_dataset_is_an_error() {
        let (sources, diags) = from("[x]\nadapter = \"demo_bus\"\ndataset = \"risk_snapshot\"\ndocument = \"cvi_params\"\ntopics = [\"a/>\"]\n");
        assert!(sources.is_empty());
        assert!(diags.iter().any(|d| d.path.as_deref() == Some("sources.x.dataset") && d.message.contains("document family")));
    }
```

- [ ] **Step 2: Run to verify failure** — compile errors / assertion failures.

- [ ] **Step 3: Implement**

`parse_duration`: trim; if `s == "0"` → `Some(ZERO)`; if it ends with `ms` → digits before are millis; else the existing s/m/h rule. In `from_doc`, read `adapter` (string, default `CSV_DIR_ADAPTER`); if subscribed: `document` (missing → Error at `.document`, skip), `topics` (missing/empty/non-string → Error at `.topics`, skip), `coalesce` via the `duration` closure with default 500 ms (its message now says "s, m, h or ms"), `source_time` (`"receive"` | `"document:<field>"` | absent → Receive; anything else → Warning at `.source_time`, Receive), and the dataset must be `is_document()` (else Error at `.dataset`, skip); `paths`/`readiness`/`poll_interval`/`pending_timeout`/`batch_pattern` present → Warning at that key, ignored. If not subscribed: everything as today, and `document`/`topics`/`coalesce`/`source_time` present → Warning, ignored. The existing "missing or empty 'paths'" Error stays for directory sources only.

- [ ] **Step 4: Run to verify pass** — `cargo test -p geode-core source_config` → PASS; `cargo test --workspace` (the Sources dialog in `geode-shell` constructs `SourceSpec`? — it reads through `from_doc`; check `cargo check --workspace --all-targets` for struct literals to fill: `geode-app/src/bridge.rs` tests build `SourceSpec { .. }` — add the five fields with defaults).

- [ ] **Step 5: Mutation entries and commit** — one each for: the `topics` Error (skip), the `document` Error, the measure-dataset Error, the `ms` unit, the bare-zero rule, the subscribed-source `paths` Warning. Commit: `git commit -m "sources: adapter, document, topics, coalesce, source_time; parse_duration takes ms"`.

---

### Task 6: `geode_data::adapter` — traits, registry, sinks, topics, and the channel adapter

**Files:**
- Create: `crates/geode-data/src/adapter/mod.rs`, `crates/geode-data/src/adapter/topic.rs`, `crates/geode-data/src/adapter/channel.rs`
- Modify: `crates/geode-data/src/lib.rs` (`pub mod adapter;`)
- Test: each file's test module

**Interfaces:**
- Produces (`geode_data::adapter`):

```rust
pub struct Message { pub topic: String, pub received: DateTime<Utc>, pub bytes: Vec<u8> }

/// Bounded, never blocking. `push` returns false and counts when full or closed.
#[derive(Clone)]
pub struct MessageSink { tx: SyncSender<Message>, refused: Arc<AtomicU64> }
impl MessageSink {
    pub fn bounded(capacity: usize) -> (MessageSink, Receiver<Message>);
    pub fn push(&self, m: Message) -> bool;
    pub fn refused(&self) -> u64;
}
pub const MESSAGE_BOUND: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState { Connected, Reconnecting, Lost { reason: String } }
pub type HealthSink = Arc<dyn Fn(ConnectionState) + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterError { pub message: String }

pub trait Subscription: Send {
    fn subscribe(&mut self, topics: &[String], sink: MessageSink, health: HealthSink) -> Result<(), AdapterError>;
    fn unsubscribe(&mut self);
}
pub trait Egress: Send {
    fn upload(&mut self, target: &str, bytes: Vec<u8>) -> Result<(), AdapterError>;
}
pub trait Adapter: Send + Sync {
    fn name(&self) -> &'static str;
    fn subscription(&self) -> Option<Box<dyn Subscription>>;
    fn egress(&self) -> Option<Box<dyn Egress>>;
}

#[derive(Default, Clone)]
pub struct AdapterRegistry { adapters: HashMap<String, Arc<dyn Adapter>> }
impl AdapterRegistry {
    pub fn register(&mut self, adapter: Arc<dyn Adapter>);   // keyed by name(); replacement warns
    pub fn get(&self, name: &str) -> Option<Arc<dyn Adapter>>;
    pub fn names(&self) -> Vec<String>;
}

// adapter::topic
pub fn topic_matches(pattern: &str, topic: &str) -> bool;

// adapter::channel
pub struct ChannelAdapter { .. }            // implements Adapter, name() == "channel" by default; `with_name(&'static str)`
pub struct ChannelFeed { .. }               // Clone; the producer side
impl ChannelAdapter {
    pub fn new(name: &'static str) -> (Arc<ChannelAdapter>, ChannelFeed);
}
impl ChannelFeed { pub fn publish(&self, topic: &str, bytes: Vec<u8>) -> bool; pub fn set_state(&self, state: ConnectionState); }
```

Semantics of `ChannelAdapter`: `subscription()` hands back a `Box<dyn Subscription>` that, on `subscribe(topics, sink, health)`, registers `(topics, sink, health)` with the adapter and immediately calls `health(Connected)`. The adapter runs one dispatcher thread, started on first `subscribe`, draining the feed's inbound channel: each message is pushed (cloned) to every registered subscription whose topic list has a pattern matching `message.topic`; `set_state` fans the state out to every registered health sink. `egress()` hands back an `Egress` whose `upload(target, bytes)` publishes `Message { topic: target, received: now, bytes }` on the same inbound feed — the echo (§5.5, §9.4). `unsubscribe` removes the registration. Dropping the last `ChannelFeed` closes the channel and ends the dispatcher.

- [ ] **Step 1: Write the failing tests**

`topic.rs`:

```rust
    #[test]
    fn topic_patterns_follow_solace_rules() {
        assert!(topic_matches("marketdata/cvi/>", "marketdata/cvi/SPX.Z"));
        assert!(topic_matches("marketdata/cvi/>", "marketdata/cvi/SPX.Z/extra"));
        assert!(!topic_matches("marketdata/cvi/>", "marketdata/cvi"), "> needs at least one level");
        assert!(topic_matches("marketdata/*/SPX.Z", "marketdata/cvi/SPX.Z"));
        assert!(!topic_matches("marketdata/*/SPX.Z", "marketdata/cvi/x/SPX.Z"));
        assert!(topic_matches("marketdata/cvi/SPX.Z", "marketdata/cvi/SPX.Z"));
        assert!(!topic_matches("marketdata/cvi/SPX.Z", "marketdata/cvi/NDX.Z"));
        assert!(!topic_matches("marketdata/>/cvi", "marketdata/x/cvi"), "> is only legal as the last level");
    }
```

`mod.rs`:

```rust
    #[test]
    fn a_full_sink_refuses_and_counts_rather_than_blocking() {
        let (sink, rx) = MessageSink::bounded(2);
        let m = |t: &str| Message { topic: t.into(), received: Utc::now(), bytes: vec![] };
        assert!(sink.push(m("a")) && sink.push(m("b")));
        assert!(!sink.push(m("c")));
        assert_eq!(sink.refused(), 1);
        drop(rx);
        assert!(!sink.push(m("d")));
        assert_eq!(sink.refused(), 2);
    }

    #[test]
    fn the_registry_finds_adapters_by_name_and_replaces_on_rename() { /* two ChannelAdapters, get/names/replace */ }
```

`channel.rs`:

```rust
    #[test]
    fn a_published_message_reaches_every_matching_subscription_and_no_other() {
        let (adapter, feed) = ChannelAdapter::new("demo_bus");
        let (cvi_sink, cvi_rx) = MessageSink::bounded(8);
        let (repo_sink, repo_rx) = MessageSink::bounded(8);
        let states: Arc<Mutex<Vec<ConnectionState>>> = Default::default();
        let health: HealthSink = { let s = states.clone(); Arc::new(move |st| s.lock().unwrap().push(st)) };
        let mut sub_a = adapter.subscription().unwrap();
        sub_a.subscribe(&["marketdata/cvi/>".into()], cvi_sink, health.clone()).unwrap();
        let mut sub_b = adapter.subscription().unwrap();
        sub_b.subscribe(&["marketdata/repo/>".into()], repo_sink, health.clone()).unwrap();
        assert_eq!(states.lock().unwrap().as_slice(), &[ConnectionState::Connected, ConnectionState::Connected]);
        assert!(feed.publish("marketdata/cvi/SPX.Z", b"<x/>".to_vec()));
        let m = cvi_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!((m.topic.as_str(), m.bytes.as_slice()), ("marketdata/cvi/SPX.Z", &b"<x/>"[..]));
        assert!(repo_rx.recv_timeout(Duration::from_millis(200)).is_err(), "repo did not get cvi's message");
        sub_a.unsubscribe();
        assert!(feed.publish("marketdata/cvi/SPX.Z", b"<y/>".to_vec()));
        assert!(cvi_rx.recv_timeout(Duration::from_millis(200)).is_err(), "unsubscribed");
    }

    #[test]
    fn an_upload_echoes_on_the_target_topic() {
        let (adapter, feed) = ChannelAdapter::new("demo_bus");
        let (sink, rx) = MessageSink::bounded(8);
        let mut sub = adapter.subscription().unwrap();
        sub.subscribe(&["marketdata/upload/>".into()], sink, Arc::new(|_| {})).unwrap();
        let mut egress = adapter.egress().unwrap();
        egress.upload("marketdata/upload/cvi", b"<doc/>".to_vec()).unwrap();
        let m = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!((m.topic.as_str(), m.bytes.as_slice()), ("marketdata/upload/cvi", &b"<doc/>"[..]));
        let _ = feed;
    }

    #[test]
    fn set_state_reaches_every_health_sink() { /* two subscriptions, feed.set_state(Lost{reason}), both sinks saw it */ }

    #[test]
    fn a_subscription_whose_sink_is_full_loses_the_message_and_the_dispatcher_lives() {
        // bounded(1), publish three, only the first is received, later publishes still flow after a drain.
    }
```

- [ ] **Step 2: Run to verify failure** — compile errors.

- [ ] **Step 3: Implement** — as specified. The dispatcher thread is named `geode-channel-<name>`; registrations live in `Mutex<Vec<Registration { id: u64, topics: Vec<String>, sink: MessageSink, health: HealthSink }>>`; `subscribe` from the `Box<dyn Subscription>` (which holds `Arc<ChannelAdapter>` + its registration id). `MessageSink::push` counts every refusal in `refused`. `ConnectionState` fan-out is synchronous on the caller's thread (a test thread or the feed's owner), never on the dispatcher. No sockets, no files.

- [ ] **Step 4: Run to verify pass** — `cargo test -p geode-data adapter` → PASS; clippy clean (watch `Arc<Mutex<..>>` lock-in-loop warnings).

- [ ] **Step 5: Mutation entries and commit** — entries for: `>` requiring at least one level (`topic.rs`), `*` matching exactly one level, a non-matching subscription NOT receiving (the topic check in the dispatcher → `true`), `unsubscribe` removing the registration, egress publishing on `target` (→ a fixed topic). Commit: `git commit -m "data: the adapter tier — Subscription/Egress/Adapter traits, registry, MessageSink, topic patterns, ChannelAdapter"`.

---

### Task 7: The coalescer

**Files:**
- Create: `crates/geode-data/src/ingest/coalesce.rs`
- Modify: `crates/geode-data/src/ingest/mod.rs` (`pub mod coalesce;`)
- Test: its test module

**Interfaces:**
- Produces:

```rust
/// Per-key latest-wins with a minimum spacing between releases (spec §5.4
/// step 2). Pure: every method takes `now`, so the receiver thread's timer
/// is the only clock and the tests need none.
pub struct Coalescer<T> {
    window: Duration,
    pending: HashMap<String, T>,          // at most one per key — latest wins
    last_release: HashMap<String, Instant>,
    due_at: BTreeMap<Instant, Vec<String>>,   // when each pending key may go
}
impl<T> Coalescer<T> {
    pub fn new(window: Duration) -> Self;
    /// Offer a document for `key`. Released immediately (returned) when the
    /// window is zero or `now - last_release[key] >= window`; otherwise held,
    /// replacing any pending one, due at `last_release[key] + window`.
    pub fn offer(&mut self, now: Instant, key: String, item: T) -> Option<(String, T)>;
    /// Everything whose due time is at or before `now`, released.
    pub fn due(&mut self, now: Instant) -> Vec<(String, T)>;
    pub fn next_deadline(&self) -> Option<Instant>;
    pub fn pending(&self) -> usize;
}
```

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn a_zero_window_releases_every_offer_immediately() {
        let mut c = Coalescer::new(Duration::ZERO);
        let t0 = Instant::now();
        assert_eq!(c.offer(t0, "SPX".into(), 1), Some(("SPX".into(), 1)));
        assert_eq!(c.offer(t0, "SPX".into(), 2), Some(("SPX".into(), 2)));
        assert_eq!(c.pending(), 0);
        assert_eq!(c.next_deadline(), None);
    }

    #[test]
    fn within_the_window_the_latest_wins_and_is_released_on_the_deadline() {
        let mut c = Coalescer::new(Duration::from_millis(500));
        let t0 = Instant::now();
        assert_eq!(c.offer(t0, "SPX".into(), 1), Some(("SPX".into(), 1)), "first offer for a key goes at once");
        assert_eq!(c.offer(t0 + Duration::from_millis(100), "SPX".into(), 2), None);
        assert_eq!(c.offer(t0 + Duration::from_millis(200), "SPX".into(), 3), None);
        assert_eq!(c.pending(), 1);
        assert_eq!(c.next_deadline(), Some(t0 + Duration::from_millis(500)));
        assert!(c.due(t0 + Duration::from_millis(499)).is_empty());
        assert_eq!(c.due(t0 + Duration::from_millis(500)), vec![("SPX".into(), 3)]);
        assert_eq!(c.pending(), 0);
        // The release restarts the window from the release instant.
        assert_eq!(c.offer(t0 + Duration::from_millis(600), "SPX".into(), 4), None);
        assert_eq!(c.next_deadline(), Some(t0 + Duration::from_millis(1000)));
    }

    #[test]
    fn keys_coalesce_independently() {
        let mut c = Coalescer::new(Duration::from_millis(500));
        let t0 = Instant::now();
        c.offer(t0, "SPX".into(), 1);
        assert_eq!(c.offer(t0, "NDX".into(), 10), Some(("NDX".into(), 10)));
        assert_eq!(c.offer(t0 + Duration::from_millis(10), "SPX".into(), 2), None);
        assert_eq!(c.offer(t0 + Duration::from_millis(20), "NDX".into(), 11), None);
        let mut d = c.due(t0 + Duration::from_millis(500));
        d.sort();
        assert_eq!(d, vec![("NDX".into(), 11), ("SPX".into(), 2)]);
    }

    #[test]
    fn an_offer_after_the_window_elapsed_with_nothing_pending_goes_at_once() {
        let mut c = Coalescer::new(Duration::from_millis(500));
        let t0 = Instant::now();
        c.offer(t0, "SPX".into(), 1);
        assert_eq!(c.offer(t0 + Duration::from_secs(2), "SPX".into(), 2), Some(("SPX".into(), 2)));
    }
```

- [ ] **Step 2: Run to verify failure** — compile error. **Step 3: Implement** as specified (`due_at` keyed by `Instant`; a key re-offered while pending keeps its existing due time; `due` drains every entry `<= now`, sets `last_release` to `now` for each). **Step 4: Verify** — PASS.

- [ ] **Step 5: Mutation entries and commit** — entries for: latest-wins (`pending.insert` → keep the first, i.e. `entry().or_insert`), the release restarting the window (`last_release.insert(key, now)` removed), independent keys (window keyed globally). Commit: `git commit -m "ingest: Coalescer — per-key latest-wins with a minimum release spacing"`.

---

### Task 8: The runner takes documents

**Files:**
- Modify: `crates/geode-data/src/ingest/runner.rs` (`Queue`, `IngestHandle::submit_document`, the `run` loop, module doc)
- Test: its test module
- Modify: `scripts/mutation-check.sh` (`runner.rs` is anchored — `--anchors-only` after every edit)

**Interfaces:**
- Produces:

```rust
/// A parsed document waiting to publish (spec §5.4 step 3). Owned rows:
/// the receiver thread hands them over and keeps nothing.
pub struct DocumentJob {
    pub source: String,
    pub dataset: String,
    pub rows: DocumentRows,
    pub source_time: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
    pub bytes: u64,
}
impl IngestHandle { pub fn submit_document(&self, job: DocumentJob) }
```

Semantics: `Queue` gains `documents: VecDeque<DocumentJob>`. The run loop pops a document **ahead of** any file item. Ruling (spec §5.4 says documents take the latest-other rung): a document publish is milliseconds and already coalesced upstream, so popping it first cannot starve a file load and spares the runner a second priority vocabulary; the rung wording in §5.4 is amended in Task 11. The document arm resolves the dataset (undeclared → `Failed` naming it, like the file arm), then runs `publish_document` under the same `catch_unwind` + `geode_core::panic::contained` boundary the file arm uses, and emits `IngestEvent::Published { source, dataset, batch: published.batch, gen_id, books: vec![None], rows, health: Health::Ok }` or `IngestEvent::Failed { .., batch: join_key(&job.rows.key), reason }`. `in_flight` is untouched by documents (it is the file dedupe's key). `PlanComplete` fires when both queues are empty.

- [ ] **Step 1: Write the failing tests** (in `runner.rs` tests; use `IngestRunner::spawn_channel` with a store that has `cvi_dataset()` applied — `crate::store::ddl::tests_support`)

```rust
    #[test]
    fn a_submitted_document_publishes_and_reports_its_batch() {
        let (dir, store) = document_store();   // tempdir + Store with cvi_dataset applied + catalog tables
        let mut schema = SchemaSpec::default(); schema.datasets.push(cvi_dataset());
        let (handle, rx) = IngestRunner::spawn_channel(store, schema);
        handle.submit_document(DocumentJob { source: "cvi".into(), dataset: "cvi_params".into(), rows: cvi_doc("SPX.Z", [1.,2.,3.,4.,5.,6.]), source_time: ts("2026-09-12T14:00:00Z"), received_at: ts("2026-09-12T14:00:00Z"), bytes: 10 });
        match next_event(&rx) {
            IngestEvent::Published { source, dataset, batch, books, rows, health, .. } => {
                assert_eq!((source.as_str(), dataset.as_str(), batch.as_str()), ("cvi", "cvi_params", "SPX.Z"));
                assert_eq!((books, rows, health), (vec![None], 6, Health::Ok));
            }
            other => panic!("{other:?}"),
        }
        handle.shutdown();
        let _ = dir;
    }

    #[test]
    fn an_invalid_document_fails_by_batch_and_the_runner_lives() {
        // zero rows → Failed { batch: "SPX.Z", reason contains "document has no rows" }, then a valid one still publishes
    }

    #[test]
    fn a_document_for_an_undeclared_dataset_fails_naming_it() { .. }

    #[test]
    fn a_document_is_popped_ahead_of_a_queued_file() {
        // submit a WorkPlan with one file item AND a document; the first event is the document's Published.
        // (Use the existing test helpers that build a real CSV WorkPlan from the demo generator — see the runner's own tests.)
    }

    #[test]
    fn a_panicking_publish_is_contained_and_reported() {
        // spawn_channel_with_load's shape exists for files; add a `spawn_channel_with_publish` test door with a PublishFn
        // that panics, assert Failed { reason contains "panicked" } and that the next document still publishes.
    }
```

- [ ] **Step 2: Run to verify failure** — compile errors.

- [ ] **Step 3: Implement** — as specified. Introduce `type PublishFn = fn(&Store, &DocumentPublishRequest) -> Result<DocumentPublished, StoreError>` beside `LoadFn` and thread it through `spawn_with_load` (rename to `spawn_with` taking both) so the panic test can inject one. Rewrite the module doc's "priority-ordered queue" sentence to describe both queues and why documents go first. Every existing runner test must pass unchanged.

- [ ] **Step 4: Run to verify pass** — `cargo test -p geode-data ingest::runner` → PASS; `zsh scripts/mutation-check.sh --anchors-only` → 0 (re-anchor anything the loop edit moved).

- [ ] **Step 5: Mutation entries and commit** — entries for: documents popped first (swap the order), `Failed.batch` from `join_key` (→ `"doc"`), `books: vec![None]` (→ `vec![]`), the containment boundary on publish (remove `contained`, named test the panic one). Commit: `git commit -m "ingest runner: a document queue popped ahead of files, published under the same panic boundary"`.

---

### Task 9: The receiver pipeline and the service wiring

**Files:**
- Create: `crates/geode-data/src/ingest/subscribe.rs`
- Modify: `crates/geode-data/src/ingest/mod.rs`
- Modify: `crates/geode-data/src/service.rs` (`DataServiceConfig`, `DataService` fields, `open`, `shutdown`)
- Modify: `crates/geode-data/src/handle.rs` tests and every `DataServiceConfig { .. }` literal (`..Default::default()` is not available — add the two fields explicitly, `AdapterRegistry::default()` / `DocumentRegistry::default()`)
- Test: `subscribe.rs` and `service.rs` test modules
- Modify: `scripts/mutation-check.sh` (`service.rs` is anchored)

**Interfaces:**
- Produces:

```rust
// service.rs
pub struct DataServiceConfig { .., pub adapters: AdapterRegistry, pub documents: DocumentRegistry }
struct DataService { .., subscriptions: Vec<SubscriptionWorker> }   // dropped/shut down before the scheduler

// ingest/subscribe.rs
pub struct SubscriptionWorker { subscription: Box<dyn Subscription>, stop: Arc<AtomicBool>, thread: Option<JoinHandle<()>> }
impl SubscriptionWorker {
    /// Subscribes and starts the receiver thread. `emit_load` is the ingest
    /// sink's load-lane closure shape; `emit_discovery` the scheduler's.
    pub fn spawn(spec: &SourceSpec, dataset: DatasetSpec, kind: Arc<dyn DocumentKind>, subscription: Box<dyn Subscription>,
                 ingest: Arc<IngestHandle>, on_parse_failure: Arc<dyn Fn(&str /*batch-or-topic*/, String /*reason*/) + Send + Sync>,
                 on_connection: HealthSink) -> Result<SubscriptionWorker, AdapterError>;
    pub fn shutdown(&mut self);   // unsubscribe, stop flag, join
}
/// Pure: the source time for a parsed document under the source's policy.
pub fn source_time_of(policy: &SourceTime, rows: &DocumentRows, received: DateTime<Utc>) -> Result<DateTime<Utc>, String>;
```

The receiver thread (named `geode-subscribe-<source>`): `MessageSink::bounded(MESSAGE_BOUND)` → `subscription.subscribe(&spec.topics, sink, on_connection)`; loop `rx.recv_timeout(until next coalescer deadline, capped at 250 ms)`; on message: `kind.parse(&bytes)` — `Err` → `on_parse_failure(&message.topic, format!("parse: {e}"))`; `Ok(parsed)` → log each `unknown_paths` entry not yet seen for this source at `warn` (`target: "geode::ingest"`, "source {name}: unknown element {path} in {kind} document; skipped") and remember it in a `HashSet<String>`; `parsed.rows.validate(&dataset)` — `Err` → `on_parse_failure(&join_key(&rows.key), e)`; `source_time_of` — `Err` → same; then `coalescer.offer(now, join_key(&rows.key), Pending { rows, received, bytes })` → each release → `ingest.submit_document(DocumentJob{..})`; after every message and on every timeout, `coalescer.due(now)` → submit each. On `stop`, `unsubscribe` and return. `source_time_of`: `Receive` → `received`; `Document(field)` → the attribute named `field` — a `Date` becomes midnight UTC of that date, a `Utf8` is parsed as RFC 3339 (else Err naming the field and text), any other type or a missing attribute is `Err`.

`DataService::open`: after the pool and the ingest runner are up and before `Scheduler::spawn`, partition `config.sources`: `is_subscribed()` ones each get, in order: `config.adapters.get(&spec.adapter)` (None → `health_tracker.report_discovery_and_emit(name, Failed { reason: "adapter '<a>' is not in this build" }, ..)` through the scheduler-sink emit closure shape, and skip), `config.documents.get(document)` (None → Failed "document kind '<k>' is not registered"), `check_kind_against(kind, ds)` (Err → Failed with the message), `adapter.subscription()` (None → Failed "adapter '<a>' has no subscription side"), then `SubscriptionWorker::spawn(..)` whose `on_parse_failure` calls `health_tracker.report_load_and_emit(name, batch, Failed{reason}, format!("{batch}: {reason}"), emit)` with the SAME emit closure the ingest sink's `Failed` arm uses (copy it verbatim; `log_ingest_failure` too), and whose `on_connection` maps `Connected → Ok`, `Reconnecting → Pending`, `Lost{reason} → Failed{reason}` into `report_discovery_and_emit` with the scheduler sink's emit closure. Directory sources go to `Scheduler::spawn` as today. `shutdown` shuts subscriptions down first (they submit into the runner), then pool, scheduler, ingest as today. A source whose adapter name is `csv_dir` never touches the registry.

- [ ] **Step 1: Write the failing tests**

`subscribe.rs` tests: `source_time_of` for each policy branch (Receive; Document over a Date attribute; Document over an RFC-3339 Utf8; missing field Err; wrong type Err). A worker test over a `ChannelAdapter`: build a store with `cvi_dataset`, a real `IngestRunner::spawn_channel`, a `SubscriptionWorker` with `CviKind`-shaped fake kind (a local `FakeKind` whose `parse` decodes a tiny custom byte format into `cvi_doc(key, params)`; the real CVI kind lives in another crate and `geode-data` must not depend on it — write the fake in `tests_support` for Task 9 and Task 10's service tests), publish two messages for `SPX.Z` inside one 200 ms window and one for `NDX.Z`, assert the runner receives exactly two `Published` events (NDX first or SPX first is fine) and the SPX one carries the second message's params; publish garbage bytes and assert `on_parse_failure` was called with the topic; `feed.set_state(Lost{..})` reaches `on_connection`.

`service.rs` tests (extend the Part 1 `document_service()` fixture family): open a service whose config has `adapters` = a `ChannelAdapter` named `demo_bus` and `documents` = the fake kind, and `sources` = one subscribed `SourceSpec` (`coalesce: Duration::ZERO`); publish on the feed; assert `DataEvent::Published { dataset: "cvi_params", batch: "SPX.Z", books: [None], .. }` arrives, then `Request::Document` returns those rows. Second test: a source naming an unregistered adapter → `DataEvent::Health { source, worst: Failed { reason contains "not in this build" }, .. }` arrives and the service still serves queries. Third: kind/dataset column mismatch (a fake kind with an extra column) → Health Failed with the mismatch message. Fourth: `feed.set_state(ConnectionState::Lost { reason: "x" })` → `DataEvent::Health { worst: Failed .. }`; then a publish still lands (load lane independent of discovery lane) and `set_state(Connected)` does NOT clear a prior parse failure's Failed (publish garbage first, then Connected: the next `Health` event, if any, is not `Ok`).

- [ ] **Step 2: Run to verify failure** — compile errors (module, fields).

- [ ] **Step 3: Implement** — as specified. Keep the receiver loop allocation-light: reuse one `HashSet<String>` for unknown paths, one `Coalescer`, no per-message `String` beyond the key.

- [ ] **Step 4: Run to verify pass** — `cargo test -p geode-data` → PASS; `cargo test --workspace` (every `DataServiceConfig` literal compiles: `handle.rs`, `service.rs` tests, `geode-app/src/bridge.rs`, `geode-app/src/main.rs` tests); `--anchors-only` → 0.

- [ ] **Step 5: Mutation entries and commit** — entries for: a missing adapter is reported and skipped (skip → continue without report); the kind/dataset check (removed); parse failure → load lane (→ discovery lane); `Connected` → `Ok` (→ `Pending`); `source_time_of` Receive (→ `Utc::now()`... use a fixed wrong value) ; the coalescer being consulted (`offer` result ignored → submit every message; the two-messages-in-a-window test catches it). Commit: `git commit -m "data: subscribed sources — SubscriptionWorker, source_time_of, DataService partitions sources by adapter"`.

---

### Task 10: The demo bus

**Files:**
- Create: `crates/geode-demo-data/src/documents.rs` (`pub mod cvi`)
- Modify: `crates/geode-demo-data/Cargo.toml` (`geode-core.workspace = true`), `crates/geode-demo-data/src/lib.rs`
- Create: `crates/geode-app/src/demo_bus.rs`
- Modify: `crates/geode-app/src/demo.rs` (`layer` gains the `[cvi]` source), `crates/geode-app/src/bridge.rs` (`data_setup` fills `documents` with `geode_documents::builtin_kinds()`; `adapters` from a new parameter), `crates/geode-app/src/main.rs` (demo mode: create the `ChannelAdapter`, register it, spawn the bus, stop it on exit), `crates/geode-app/Cargo.toml` (`geode-documents`)
- Test: `documents.rs`, `demo.rs`, `demo_bus.rs` tests

**Interfaces:**
- Produces:

```rust
// geode_demo_data::documents::cvi
pub const NODES: [f64; 12] = [-20.0, -15.0, -10.0, -5.0, -2.5, -1.0, -0.5, 0.0, 0.5, 1.0, 2.0, 3.5];
pub struct CviGenerator { .. }
impl CviGenerator {
    /// Seeded; `anchor` is the business date every document carries.
    pub fn new(seed: u64, underlyings: Vec<String>, anchor: NaiveDate) -> CviGenerator;
    pub fn underlyings(&self) -> &[String];
    /// The next document for `key`: the same node ladder, eight listed
    /// expiries (third Fridays from `anchor`), spot_ref per underlying, and
    /// params that drift by a seeded random walk from the previous call for
    /// the same key — so successive documents differ visibly.
    pub fn next_document(&mut self, key: &str) -> DocumentRows;
}

// geode_app::demo_bus
pub struct DemoBus { stop: Arc<AtomicBool>, thread: Option<JoinHandle<()>> }
pub fn spawn(feed: ChannelFeed, kind: Arc<dyn DocumentKind>, mut generator: CviGenerator, cadence: Duration, jitter: Duration, seed: u64) -> DemoBus;
impl DemoBus { pub fn stop(&mut self) }   // also on Drop
```

The bus thread: for each underlying in turn, sleeps `cadence ± jitter` (seeded, per key), then `feed.publish(&format!("marketdata/cvi/{key}"), kind.write(&generator.next_document(key)).unwrap())`; checks `stop` between publishes; on start it publishes every key once immediately so the panel (Part 3) has something on first paint.

- [ ] **Step 1: Write the failing tests**

`documents.rs`: `same_seed_same_documents` (two generators, same calls → equal `DocumentRows`); `successive_documents_drift` (two calls for one key differ in `param` but share `term`/`node`/`spot_ref`); `the_grid_is_full_and_term_major` (rows == 8 × 12, terms sorted, `validate` against a `cvi_params` dataset built from the spec's TOML through `SchemaSpec::from_doc` passes — `geode-core`'s `config::test_support` may need the `test-support` feature; if so, hand-build the `DatasetSpec` instead).

`demo.rs`: `the_demo_layer_declares_the_cvi_source` — `layer(..)`'s `sources` doc has `[cvi]` with `adapter = "demo_bus"`, `dataset = "cvi_params"`, `document = "cvi_params"`, `topics = ["marketdata/cvi/>"]`, `coalesce = "500ms"`, `priority = "latest_other"`; and `SourceSpec::from_doc` over the demo `datasets` + `sources` docs yields two sources with no diagnostics.

`demo_bus.rs`: `the_bus_publishes_every_key_once_at_start_then_on_its_cadence` — a `ChannelAdapter` + subscription with a `MessageSink::bounded(64)`, `spawn(.., cadence 50ms, jitter 10ms)`, receive ≥ 2×keys messages within 2 s, every topic is `marketdata/cvi/<key>`, and `CviKind.parse` of each body succeeds with `rows.key == [key]`; `stop()` returns promptly and no message arrives afterwards.

- [ ] **Step 2: Run to verify failure** — compile errors.

- [ ] **Step 3: Implement** — `CviGenerator` with `rand::rngs::StdRng::seed_from_u64`, one walk state per key (`HashMap<String, Vec<f64>>`), spot_ref from a per-underlying base (SPX 7650, NDX 22000, RUT 2300, others 1000 + hash) drifting ±0.2% per call; third-Friday helper is a small pure function with its own test. `demo_bus::spawn` as specified (thread `geode-demo-bus`). In `main.rs` demo mode: `let (adapter, feed) = ChannelAdapter::new("demo_bus"); adapters.register(adapter);` before `bridge::data_setup`, and after the bridge starts, `demo_bus::spawn(feed, Arc::new(CviKind), CviGenerator::new(42, generator's own underlying vocabulary — `geode_demo_data` exposes it; add `pub fn demo_underlyings() -> Vec<String>` if it does not`, today's date), Duration::from_secs(5), Duration::from_secs(2), 42)`; keep the `DemoBus` alive until the app exits (stop on the existing shutdown path where `_log_guard` is dropped). `bridge::data_setup(config, db, adapters: AdapterRegistry)` — non-demo passes `AdapterRegistry::default()`; `documents` is always `builtin_kinds()` folded into a `DocumentRegistry`.

- [ ] **Step 4: Verify** — `cargo test --workspace` → PASS; `cargo run -p geode-app -- --demo 1000` boots (no display in the sandbox: confirm through the `demo` tests and, if a display is available, that the diagnostics tile's data section shows `cvi_params` partitions arriving); clippy clean on both platforms' code paths (no platform-specific code).

- [ ] **Step 5: Mutation entries and commit** — entries for: the initial publish-every-key-once (removed), the topic format (`marketdata/cvi/{key}` → fixed), the generator's drift (walk step → 0, named `successive_documents_drift`), the demo layer's `[cvi]` source (removed, named the layer test). Commit: `git commit -m "demo: CVI document generator and the demo bus over the channel adapter; the demo layer subscribes cvi_params"`.

---

### Task 11: Documentation, spec as-built, and the numbers

**Files:**
- Modify: `CLAUDE.md` (a "Market-data documents Part 2" paragraph after Part 1's; the `--demo` gotcha; the Commands block gains `cargo bench -p geode-documents`)
- Modify: `docs/superpowers/specs/2026-09-12-geode-market-data-documents-design.md` (§5.4 rung wording amended to the Task 8 ruling; a new §5.6 "As built (Part 2)" recording: documents popped ahead of files; `SourceTime::Document` reads a Date attribute as midnight UTC or a Utf8 as RFC 3339; the coalescer's window restarts from the release; `ConnectionState` mapping; the `DocumentRegistry`/`AdapterRegistry` live in the service config and are filled by the app; the demo bus publishes every key once at start)
- Modify: `docs/perf.md` (a "Market-data documents" section with the Task 4 bench numbers for parse and write at 20×30 and 200×300, and `publish_document` per document measured by a new criterion bench `crates/geode-data/benches/publish_document.rs` at the same two sizes — add it with `harness = false`; archive growth at the demo cadence computed from the bench's bytes-per-generation and 5 s per key)
- Modify: `scripts/mutation-check.sh` header ("the adapter tier, the coalescer, the receiver pipeline")
- Modify: the roadmap spec's §6 slice 1 line to note Part 2 done and Parts 3–4 remaining

- [ ] **Step 1: Write the bench** (`benches/publish_document.rs`: temp store with `cvi_dataset` applied, `publish_document` of a 20×30 and a 200×300 `DocumentRows`, criterion `iter_batched` with a fresh key per iteration so live replacement is measured too). `cargo bench --workspace --no-run` compiles it; run `cargo bench -p geode-documents` and `cargo bench -p geode-data publish_document` once and record the p50s.
- [ ] **Step 2: Docs** as listed; every sentence checked against the code (a contradicting sentence is a defect).
- [ ] **Step 3: Full gates, then the harness detached over changed files**, then commit: `git commit -m "docs: Part 2 of market-data documents — CLAUDE.md, spec §5.6, perf numbers, harness header"`.
- [ ] **Step 4: Hand off** per `superpowers:finishing-a-development-branch`.

---

## Self-review

**Spec coverage (§5, §6, §10, §12 part 2):**
- §5.1 config → Task 5 (all six keys, defaults, the warnings on foreign keys, the document-family requirement).
- §5.2 traits → Task 6 (`Message`, `Subscription`, `Egress`, `Adapter`, `MessageSink` counting refusals, `HealthSink` with the three states).
- §5.3 registry, missing adapter → failed source, directory adapter unconditional → Tasks 6 and 9 (`csv_dir` bypasses the registry).
- §5.4 pipeline: parse → Task 9; coalesce → Task 7 (pure, `now`-parameterised); enqueue as a work item → Task 8 (a separate document queue, ruled; amended in Task 11); connection state → lanes mapping → Task 9.
- §5.5 channel adapter with echo → Task 6.
- §6.1 crate → Task 4; §6.2 vocabulary → Task 3 (`ParsedDocument` carries `unknown_paths`); §6.3 CVI rules (ragged, unknown-once, missing, write round-trip) → Tasks 4 and 9 (the once-per-source logging is the receiver's); §6.4 registration and the column check → Tasks 3 and 9.
- §10 demo generator, bus, layer → Task 10; the demo-DB rule stands (no schema change here).
- §11's Part 2 tests: coalescer, health lanes, missing adapter/kind, column mismatch, parser property test, unknown/missing elements — Tasks 4, 7, 9; benchmarks → Task 11.
- §12 part 2 done state ("`--demo` publishes documents into the database with no panel yet; the diagnostics tile's data section shows them") → Task 10 Step 4.
- Part 1 parked items → Tasks 1 and 2.

**Placeholders:** Task 3, 4, 6, 8 and 9 mutation entries say "copy the line verbatim once written" for anchors on code that does not yet exist — deliberate, with the mutation meaning stated each time. Task 1's helper names (`compile_distinct_with_cache`, `cache.matches`, `like_pattern`, `lower_expression_for`) are marked "adapt to the real name" because the measure path's private helpers must be read, not guessed; the assertions are exact.

**Type consistency:** `DocumentKind`/`ParsedDocument`/`check_kind_against` (Task 3) are what Tasks 4, 9 and 10 use; `MessageSink::bounded`, `HealthSink`, `ConnectionState`, `ChannelAdapter::new -> (Arc<ChannelAdapter>, ChannelFeed)`, `ChannelFeed::{publish, set_state}` (Task 6) are what Tasks 9 and 10 use; `Coalescer::{offer, due, next_deadline}` (Task 7) is what Task 9 drives; `DocumentJob` and `submit_document` (Task 8) are what Task 9 submits; `SourceSpec::{adapter, document, topics, coalesce, source_time, is_subscribed}` and `SourceTime` (Task 5) are what Task 9 reads and Task 10's layer declares; `DataServiceConfig::{adapters, documents}` (Task 9) is what Task 10's `data_setup` fills.
