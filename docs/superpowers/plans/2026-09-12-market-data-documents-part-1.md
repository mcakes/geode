# Market-Data Documents, Part 1 — The Document Family, Storage and the Document Request

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A dataset can declare `family = "document"` with an identity key, ordered axes, value columns and document-level attributes; a parsed document publishes as a generation of that dataset under the existing live/archive, as-of, retention and catalog machinery; and a tile can ask the data handle for one document by key and receive an ordinary query outcome.

**Architecture:** The measure family and its grain vocabulary are untouched. The document family is a second branch in `DatasetSpec` (`Family`, `key`, `axes`, two new `ColumnRole`s), a second table shape in `store::ddl` reached through a shared `TablePair` that `publish_file` and `retention::sweep` are refactored to take instead of a `Grain`, a `publish_document` door that stages `geode_core::document::DocumentRows` through a DuckDB appender and then runs the same publish transaction, and a `Request::Document` the service compiles to a plain select ordered by the axes and submits to the query pool exactly like a view query. Everything here is headless: no gpui, no adapter, no parser — those are Parts 2 to 4.

**Tech Stack:** Rust, DuckDB 1.10505 via `duckdb-rs` (`bundled`, `chrono`), Arrow 58, `toml`, `scripts/mutation-check.sh`.

**Spec:** `docs/superpowers/specs/2026-09-12-geode-market-data-documents-design.md` — **§3, §4, §7 and §12 part 1 are this plan's whole brief**; §5, §6, §8, §9 are Parts 2–4 and are background only. **Also binding:** `docs/superpowers/specs/2026-09-12-geode-modules-roadmap.md` rulings 1 and 7, `docs/superpowers/specs/2026-08-30-geode-phase-2-data-design.md` §4.3–§4.6 (publication, generations, backfill guard, retention) and §6.5 (as-of routing), and `docs/PHILOSOPHY.md` §6 (no row objects; struct-of-arrays).

## Global Constraints

- CI runs on **macOS and Windows**: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo bench --workspace --no-run`, `cargo check -p geode-shell --features test-support --all-targets`. Run all five before every commit that touches Rust.
- **TDD**: write the failing test, run it, watch it fail for the right reason, then implement.
- **A mutation entry for every behaviour changed**, appended to `scripts/mutation-check.sh` immediately after the last `run_mutation` entry (currently `dialog: enter_filter_by_mouse ignores the settings dialog` at line ~7471, before the `if [[ -n "$changed_ref" ]]` block at ~7478), each naming the test expected to catch it as its 6th argument. **Commit before mutating.** Before adding an anchor, confirm it occurs exactly once as a substring of its file (`grep -c -F '<anchor>' <file>` must print `1`). `zsh scripts/mutation-check.sh --anchors-only` must exit 0 before every commit — a refactor silently orphans existing anchors in any file you touch (Task 6 touches `publish.rs`, `retention.rs`, `ddl.rs` and `load.rs`, all heavily anchored: run `--anchors-only` after every edit there).
- **Run the harness DETACHED** (`nohup zsh scripts/mutation-check.sh --changed=main > /tmp/mut.log 2>&1 &`), never in a timed foreground call. Verify `git status --porcelain` is clean afterwards.
- `geode-shell` never depends on `geode-data`; `geode-data` and `geode-demo-data` never depend on each other except `geode-data`'s existing dev-dependency on the generator — which is why `DocumentRows` goes in `geode-core`, not `geode-data`.
- No row objects: `DocumentRows` is struct-of-arrays and the staging appender iterates columns by index; nothing here builds a `Vec<Row>`.
- Every diagnostic a reader emits fills `Diagnostic.path` with the offending TOML key (Part 2b's rule), so the schema inspector lands it on the right row.
- Doc comments explain WHY, densely. **A comment that contradicts the code is a defect.** `store::ddl`'s "Create the live and archive tables for every grain the dataset declares measures at", `publish.rs`'s "Table names carry the dataset (spec §4.2), so the request must name it: two datasets can share a grain", and `retention::sweep`'s per-grain wording each become false in Task 6 and must be rewritten there.
- Table names: a document dataset's pair is `{dataset}_document_live` / `{dataset}_document_archive`. `document` is not a `Grain::short()` value, so it can never collide with a grain table of the same dataset.
- The key separator is `\u{1f}` (ASCII unit separator). It is a constant in one place (`geode_core::document::KEY_SEPARATOR`) and nothing else spells it.

## As-built vocabulary this plan builds on

```rust
// crates/geode-core/src/schema/mod.rs
pub struct DatasetSpec { pub name: String, pub columns: Vec<ColumnSpec> }
impl DatasetSpec { fn column(&self, &str) -> Option<&ColumnSpec>; fn grains(&self) -> Vec<Grain>;
                   fn groupable_columns(&self) -> Vec<&str>; fn categorical_columns(&self) -> Vec<&str>;
                   fn carries(&self, Grain, &str) -> bool; fn textual_columns(&self) -> impl Iterator<Item=&ColumnSpec> }
pub struct SchemaSpec { pub datasets: Vec<DatasetSpec> }   // from_doc(&MergedDoc) -> (SchemaSpec, Vec<Diagnostic>)
fn validate_dataset(ds: &mut DatasetSpec) -> Vec<Diagnostic>   // private; runs per dataset inside from_doc
fn parse_column(ds: &str, name: &str, value: &toml::Value) -> Result<(ColumnSpec, Option<Diagnostic>), Diagnostic>
pub const RESERVED_COLUMNS: &[&str] = &["batch", "source_file_id", "gen_id", "source_time"];

// crates/geode-core/src/schema/column.rs
pub enum ColumnType { Utf8, F64, I64, Date, Timestamp, Bool }      // ::sql(), ::parse()
pub enum ColumnRole { Key, Dimension { grain: Option<Grain> }, Measure { grain, aggregate }, Attribute { grain: Grain } }
pub struct ColumnSpec { name, source_name: Option<String>, ty, required: bool, textual: bool, categorical: bool, role }

// crates/geode-core/src/config/mod.rs
pub struct Diagnostic { pub severity: Severity, pub layer: Option<Layer>, pub file: Option<PathBuf>, pub message: String, pub path: Option<String> }

// crates/geode-core/src/attribution.rs
pub enum ScopeSemantics { Direct, SemiJoined { dimensions: Vec<String> } }   // ::is_direct(), ::meet()
pub enum Attribution { Additive, DeterminedNonAdditive, NonAttributable }

// crates/geode-core/src/scope/mod.rs
pub struct Scope { pub dimensions: Vec<DimensionSelection>, pub text: Option<String>, pub expression: Option<Expr>, pub impossible: bool }
pub struct DimensionSelection { pub column: String, pub values: Vec<String> }

// crates/geode-core/src/query.rs
pub enum AsOf { Live, At(DateTime<Utc>) }
pub struct QueryKey(pub u64);
pub struct QueryOutcome { pub key: QueryKey, pub tag: u64, pub snapshot: Result<Arc<Snapshot>, String>, pub submitted: Instant }
pub struct CatalogParams { key, tag, as_of }   // answered by DataService::catalog -> CatalogOutcome { snapshot: Result<CatalogSnapshot, String> }
pub struct PartitionCatalog { pub batch: String, pub book: Option<String>, pub generations: Vec<GenerationInfo>, pub resolved_gen: Option<i64> }

// crates/geode-core/src/snapshot.rs
pub struct ColumnMeta { pub name: String, pub attribution_by_depth: Vec<Attribution>, pub scope_semantics: ScopeSemantics }
pub struct Freshness { pub dataset: String, pub as_of: Option<String>, pub generation: i64 }
pub struct Provenance { pub datasets: Vec<Freshness>, pub as_of_request: Option<String> }
impl Snapshot { fn from_batches(Vec<RecordBatch>, Vec<ColumnMeta>, grouping: Vec<String>, Provenance) -> Result<Snapshot, ArrowError>;
                fn rows(&self) -> usize; fn f64_value(&self, name, row) -> Option<f64>; fn text_value(&self, name, row) -> Option<&str>; fn provenance(&self) -> &Provenance }

// crates/geode-data/src/store/ddl.rs
pub enum TableKind { Live, Archive }                       // ::suffix() -> "_live" | "_archive"
pub fn table_name(dataset: &str, grain: Grain, kind: TableKind) -> String   // "{dataset}_{grain.short()}{suffix}"
pub fn create_table_sql(ds: &DatasetSpec, grain: Grain, kind: TableKind) -> String
pub fn history_of(dataset: &str, ds: &DatasetSpec) -> Vec<String>          // every live+archive table of every grain
pub fn refresh_enum(conn, dataset, column, live_table, archive_table) -> Result<usize, StoreError>
pub fn enum_type_name(dataset, column) -> String; pub fn categorical_columns(ds) -> Vec<&str>

// crates/geode-data/src/store/mod.rs
pub struct Store;  impl Store { fn open(path) -> Result<Store, StoreError>; fn writer(&self) -> &Connection; fn reader(&self) -> Result<Connection, StoreError>;
                                fn apply_schema(&self, ds: &DatasetSpec) -> Result<(), StoreError> }

// crates/geode-data/src/store/publish.rs
pub struct Partition { pub batch: String, pub book: Option<String> }
pub struct PublishRequest { pub dataset: String, pub grain: Grain, pub staging_table: String, pub partitions: Vec<Partition>,
                            pub gen_id: i64, pub source_time: DateTime<Utc>, pub live_source_time: Option<DateTime<Utc>> }
pub enum PublishOutcome { Published { rows: usize }, ArchivedOnly { rows: usize, reason: String } }
pub fn publish_file(conn: &Connection, req: &PublishRequest) -> Result<PublishOutcome, StoreError>

// crates/geode-data/src/store/retention.rs
pub struct RetentionPolicy { pub keep_generations: Option<usize>, pub keep_age: Option<Duration> }
pub fn sweep(conn, ds: &DatasetSpec, grains: &[Grain], policy: &RetentionPolicy, now: DateTime<Utc>) -> Result<SweepReport, StoreError>

// crates/geode-data/src/store/catalog.rs
pub struct Catalog<'a>;  impl Catalog { fn new(&Connection) -> Catalog; fn ensure_tables(); fn reserve_file_id() -> Result<FileId>; fn reserve_gen_id() -> Result<i64>;
                                        fn record(&self, &FileGeneration) -> Result<FileId>; fn live_source_time(&self, dataset, batch, book: Option<&str>) -> Result<Option<DateTime<Utc>>>;
                                        fn dataset_as_of(&self, dataset, &[..]) -> Result<Option<DateTime<Utc>>>; fn latest_gen_id() -> Result<i64> }
pub struct FileGeneration { file_id: FileId, dataset, batch, path: PathBuf, size: u64, mtime, source_time, gen_id, loaded_at, row_count: usize,
                            books: Vec<Option<String>>, archived_only: bool, health: Health }

// crates/geode-data/src/query/as_of.rs
pub struct ResolvedGeneration { pub batch: String, pub book: Option<String>, pub gen_id: i64, pub source_time: DateTime<Utc> }
pub fn resolve_generations(conn, dataset: &str, at: DateTime<Utc>) -> Result<Vec<ResolvedGeneration>, StoreError>
pub fn generation_predicate(generations: &[ResolvedGeneration]) -> String   // "(batch = ... and book ... and gen_id = ...) or ..."

// crates/geode-data/src/query/pool.rs
pub enum RequestKind { Query, Distinct { column: String } }
pub struct QueryRequest { key, tag, submitted, view: ViewId, compiled: CompiledQuery, grouping: Vec<String>, provenance: Provenance, kind: RequestKind }
impl QueryPool { fn submit(&self, QueryRequest) -> QueryId; fn cancel(&self, QueryKey) }

// crates/geode-data/src/query/compile.rs
pub struct CompiledColumn { pub name: String, pub grain: Option<Grain>, pub attribution_by_depth: Vec<Attribution>, pub scope_semantics: ScopeSemantics }
pub struct CompiledQuery { pub sql: String, pub params: Vec<duckdb::types::Value>, pub grouping: Vec<String>, pub columns: Vec<CompiledColumn>,
                           pub stalest_input: Vec<String>, pub resolved_as_of: BTreeMap<String, DateTime<Utc>> }

// crates/geode-data/src/service.rs
pub struct DataServiceConfig { db_path, schema: SchemaSpec, views: Vec<ViewSpec>, dimensions: DerivedDimensions, query_workers: usize, sources: Vec<SourceSpec> }
pub enum DataEvent { Query(QueryOutcome), Distinct(..), Catalog(..), Published { dataset, batch, gen_id, books }, Health {..}, Polled {..}, Diagnostics(Vec<Diagnostic>) }
impl DataService { fn open(config, sink) -> Result<DataService>; fn open_channel(config) -> Result<(DataService, Receiver<DataEvent>)>;
                   fn query(&self, &QueryParams) -> Result<QueryId, StoreError>; fn catalog(&self, &CatalogParams) -> CatalogOutcome; fn cancel(&self, QueryKey); fn shutdown(self) }
// service.rs tests: fn service() -> (TempDir, TempDir, DataService, Receiver<DataEvent>); fn next(rx) -> QueryOutcome

// crates/geode-data/src/handle.rs
pub enum Request { Query(QueryParams), Distinct(DistinctParams), Catalog(CatalogParams), Cancel { key }, ReplaceViews { views, dimensions }, Shutdown }
fn serve(config, sink, rx: Receiver<Request>)    // the service thread's loop; each arm's compile error is that key's outcome
impl DataHandle { fn query(&self, QueryParams) -> bool; fn distinct(..) -> bool; fn catalog(..) -> bool; fn cancel(..) -> bool; fn for_tests() -> (DataHandle, Receiver<Request>) }
```

---

### Task 1: The document family in the schema vocabulary

**Files:**
- Modify: `crates/geode-core/src/schema/column.rs` (`ColumnRole`, `ColumnSpec::grain`, `carried_grain`)
- Modify: `crates/geode-core/src/schema/mod.rs` (`DatasetSpec`, `SchemaSpec::from_doc`, `parse_column`)
- Test: `crates/geode-core/src/schema/mod.rs` `mod tests`

**Interfaces:**
- Produces: `pub enum Family { Measures, Document }` with `Family::parse(&str) -> Option<Family>` and `Default = Measures`; `DatasetSpec { name, columns, family: Family, key: Vec<String>, axes: Vec<String> }`; `ColumnRole::Axis` and `ColumnRole::Value`; `DatasetSpec::is_document(&self) -> bool`.
- Every `DatasetSpec { .. }` literal in the workspace gains `family`, `key`, `axes` — let the compiler list them (`schema/mod.rs` tests, `geode-data` fixtures, `geode-demo-data`, `geode-blotter` tests, `geode-shell` tests). `..Default::default()` is the right fill for every existing site: they are all measure datasets.

- [ ] **Step 1: Write the failing tests**

Append to `mod tests` in `crates/geode-core/src/schema/mod.rs` (the existing `doc(text)` helper builds a `MergedDoc` from a TOML string):

```rust
    const CVI: &str = r#"
[cvi_params]
family = "document"
key = ["underlying_ref"]
axes = ["term", "node"]

[cvi_params.columns.underlying_ref]
type = "utf8"
role = "dimension"
textual = true

[cvi_params.columns.term]
type = "date"
role = "axis"

[cvi_params.columns.node]
type = "f64"
role = "axis"

[cvi_params.columns.param]
type = "f64"
role = "value"

[cvi_params.columns.anchor_date]
type = "date"
role = "attribute"

[cvi_params.columns.spot_ref]
type = "f64"
role = "attribute"
"#;

    #[test]
    fn a_document_dataset_parses_its_family_key_and_axes() {
        let (schema, diags) = SchemaSpec::from_doc(&doc(CVI));
        assert!(diags.is_empty(), "{diags:?}");
        let ds = schema.dataset("cvi_params").unwrap();
        assert_eq!(ds.family, Family::Document);
        assert!(ds.is_document());
        assert_eq!(ds.key, vec!["underlying_ref".to_string()]);
        assert_eq!(ds.axes, vec!["term".to_string(), "node".to_string()]);
        assert_eq!(ds.column("term").unwrap().role, ColumnRole::Axis);
        assert_eq!(ds.column("param").unwrap().role, ColumnRole::Value);
        // A document-level attribute carries no grain.
        assert_eq!(ds.column("spot_ref").unwrap().role, ColumnRole::Attribute { grain: None });
    }

    #[test]
    fn a_dataset_without_a_family_is_the_measure_family() {
        let (schema, _) = SchemaSpec::from_doc(&doc(SAMPLE));
        let ds = schema.dataset("risk_snapshot").unwrap();
        assert_eq!(ds.family, Family::Measures);
        assert!(!ds.is_document());
        assert!(ds.key.is_empty() && ds.axes.is_empty());
    }

    #[test]
    fn an_unknown_family_is_an_error_and_the_dataset_is_dropped() {
        let text = CVI.replace("family = \"document\"", "family = \"widget\"");
        let (schema, diags) = SchemaSpec::from_doc(&doc(&text));
        assert!(schema.dataset("cvi_params").is_none());
        let d = diags.iter().find(|d| d.message.contains("unknown family 'widget'")).unwrap();
        assert_eq!(d.severity, Severity::Error);
        assert_eq!(d.path.as_deref(), Some("cvi_params.family"));
    }

    #[test]
    fn a_measure_attribute_still_requires_its_grain() {
        // `Attribute { grain: None }` is the document reading only; on a
        // measure dataset a grainless attribute is the same missing-grain
        // error it always was.
        let text = SAMPLE.to_string()
            + "\n[risk_snapshot.columns.note]\ntype = \"utf8\"\nrole = \"attribute\"\n";
        let (_, diags) = SchemaSpec::from_doc(&doc(&text));
        assert!(diags.iter().any(|d| d.message.contains("column 'note': missing 'grain'")), "{diags:?}");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p geode-core schema::tests -- a_document_dataset_parses a_dataset_without_a_family an_unknown_family a_measure_attribute_still`
Expected: compile errors — `Family`, `ColumnRole::Axis`, `ColumnRole::Value`, `ds.family` do not exist.

- [ ] **Step 3: Add the vocabulary**

In `column.rs`, extend `ColumnRole`:

```rust
pub enum ColumnRole {
    Key,
    Dimension { grain: Option<Grain> },
    Measure { grain: Grain, aggregate: Aggregate },
    /// A non-numeric property. `Some(grain)` on a measure dataset: carried
    /// at that grain. `None` on a document dataset: document-level — one
    /// value per document, repeated on every row of it (market-data
    /// spec §3.1). `validate_dataset` refuses each reading on the other
    /// family, so `None` never reaches the grain tables.
    Attribute { grain: Option<Grain> },
    /// Document family only: identifies a row within a document, in the
    /// dataset's declared `axes` order (market-data spec §3.1).
    Axis,
    /// Document family only: a numeric cell of the document.
    Value,
}
```

Update `ColumnSpec::grain`:

```rust
    pub fn grain(&self) -> Option<Grain> {
        match self.role {
            ColumnRole::Measure { grain, .. } | ColumnRole::Attribute { grain: Some(grain) } => Some(grain),
            ColumnRole::Attribute { grain: None }
            | ColumnRole::Key
            | ColumnRole::Dimension { .. }
            | ColumnRole::Axis
            | ColumnRole::Value => None,
        }
    }
```

`carried_grain` is unchanged (its `_ => None` arm already covers the new variants). Every other `match` on `ColumnRole` in the workspace that is exhaustive (`ddl::create_table_sql`, `split.rs`, `load.rs`, `view.rs`, `groupable`-related code) now fails to compile: add `ColumnRole::Axis | ColumnRole::Value => false` (or the equivalent "not this branch" arm) at each site — the grain paths must never select an axis or value, and a document-level attribute must never be kept by `create_table_sql`'s `g == grain` test (the `Attribute { grain: Some(g) }` pattern there makes that automatic).

In `mod.rs`:

```rust
/// Which of the two dataset families a dataset belongs to (market-data
/// spec §3; roadmap ruling 7). The measure family is the grain
/// vocabulary as it always was; the document family is keyed by a
/// declared identity plus axes and has no grain at all. The two are
/// side by side rather than one declared-key model because attribution
/// rests on the grains forming a prefix chain, and nothing a document
/// dataset does needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Family {
    #[default]
    Measures,
    Document,
}

impl Family {
    pub fn parse(s: &str) -> Option<Family> {
        match s {
            "measures" => Some(Family::Measures),
            "document" => Some(Family::Document),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct DatasetSpec {
    pub name: String,
    pub columns: Vec<ColumnSpec>,
    pub family: Family,
    /// Document family: the identity key, in declared order. One document
    /// per distinct key tuple; the batch a publish replaces (spec §4.1).
    /// Empty for the measure family.
    pub key: Vec<String>,
    /// Document family: the row identity within a document, in declared
    /// order — also the order a document request sorts by (spec §7).
    /// Empty for the measure family.
    pub axes: Vec<String>,
}

impl DatasetSpec {
    pub fn is_document(&self) -> bool {
        self.family == Family::Document
    }
    // …existing methods unchanged…
}
```

In `SchemaSpec::from_doc`, before the `columns` lookup, read the three dataset-level keys. A `family` that fails to parse drops the whole dataset (`continue`) with an `Error` diagnostic whose `path` is `"{ds_name}.family"`; `key` and `axes` are read as string arrays (a non-array or non-string element is an `Error` with path `"{ds_name}.key"` / `"{ds_name}.axes"` and the dataset is dropped). On the measure family, a present `key` or `axes` is a `Warning` ("ignored on the measure family", same paths) and both are left empty.

```rust
            let family = match ds_value.get("family").and_then(|v| v.as_str()) {
                None => Family::Measures,
                Some(s) => match Family::parse(s) {
                    Some(f) => f,
                    None => {
                        diags.push(Diagnostic {
                            severity: Severity::Error,
                            layer: None,
                            file: None,
                            message: format!("dataset '{ds_name}': unknown family '{s}'; dataset dropped"),
                            path: Some(format!("{ds_name}.family")),
                        });
                        continue;
                    }
                },
            };
            let string_list = |field: &str, diags: &mut Vec<Diagnostic>| -> Result<Vec<String>, ()> {
                match ds_value.get(field) {
                    None => Ok(Vec::new()),
                    Some(v) => v
                        .as_array()
                        .and_then(|a| a.iter().map(|x| x.as_str().map(str::to_string)).collect::<Option<Vec<_>>>())
                        .ok_or_else(|| {
                            diags.push(Diagnostic {
                                severity: Severity::Error,
                                layer: None,
                                file: None,
                                message: format!("dataset '{ds_name}': '{field}' must be an array of column names; dataset dropped"),
                                path: Some(format!("{ds_name}.{field}")),
                            })
                        }),
                }
            };
            let (Ok(mut key), Ok(mut axes)) = (string_list("key", &mut diags), string_list("axes", &mut diags)) else {
                continue;
            };
            if family == Family::Measures {
                for (field, list) in [("key", &mut key), ("axes", &mut axes)] {
                    if !list.is_empty() {
                        diags.push(Diagnostic {
                            severity: Severity::Warning, layer: None, file: None,
                            message: format!("dataset '{ds_name}': '{field}' is ignored on the measure family"),
                            path: Some(format!("{ds_name}.{field}")),
                        });
                        list.clear();
                    }
                }
            }
            let mut dataset = DatasetSpec { name: ds_name.clone(), columns: Vec::new(), family, key, axes };
```

In `parse_column`, `parse_column` needs the family to know whether a grainless attribute is legal. Change its signature to `parse_column(ds: &str, family: Family, name: &str, value: &toml::Value)` and the `"attribute"` arm to:

```rust
        "attribute" => ColumnRole::Attribute {
            grain: match family {
                Family::Measures => Some(grain_of(table)?),
                Family::Document => None,
            },
        },
        "axis" => ColumnRole::Axis,
        "value" => ColumnRole::Value,
```

Also fix the `categorical_default` so `Attribute { grain: None }` is not a dimension (it already keys on `Dimension { .. }`, so no change) and confirm `grain = …` on an `axis`/`value`/document-attribute column is left to Task 2's validator rather than refused here (a stray `grain` key is silently ignored by `parse_column` today for dimensions with no grain; Task 2 makes it an error on the document family).

- [ ] **Step 4: Fix every `DatasetSpec { .. }` literal the compiler lists**

Run `cargo check --workspace --all-targets` and add `..Default::default()` (or the three explicit fields) to each literal. Do not change any of those fixtures' meaning.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p geode-core schema::tests` then `cargo test --workspace`
Expected: all PASS (the four new tests and every existing one).

- [ ] **Step 6: Commit**

```bash
git add crates/geode-core/src/schema crates/ -A
git commit -m "schema: the document family — Family, key, axes, Axis/Value roles, document-level attributes"
```

---

### Task 2: Validating a document dataset

**Files:**
- Modify: `crates/geode-core/src/schema/mod.rs` (`validate_dataset`, and a new `validate_document` it calls)
- Test: `crates/geode-core/src/schema/mod.rs` `mod tests`
- Modify: `scripts/mutation-check.sh` (append entries)

**Interfaces:**
- Consumes: Task 1's `Family`, `key`, `axes`, `ColumnRole::{Axis, Value, Attribute { grain: None }}`.
- Produces: `validate_dataset` dispatches on `ds.family`; document datasets go through `validate_document(ds: &mut DatasetSpec) -> (keep: bool, Vec<Diagnostic>)` and `from_doc` drops a dataset whose `keep` is false. Every rule in spec §3.2 is enforced, every diagnostic carries `path`.

- [ ] **Step 1: Write the failing tests**

Append to `mod tests`. Each test starts from `CVI` and breaks one rule with `str::replace`, so the fixture's other rules stay satisfied.

```rust
    fn cvi_with(from: &str, to: &str) -> (SchemaSpec, Vec<Diagnostic>) {
        let text = CVI.replace(from, to);
        assert_ne!(text, CVI, "the replacement must change the fixture");
        SchemaSpec::from_doc(&doc(&text))
    }

    fn error_with_path<'a>(diags: &'a [Diagnostic], path: &str) -> &'a Diagnostic {
        diags
            .iter()
            .find(|d| d.severity == Severity::Error && d.path.as_deref() == Some(path))
            .unwrap_or_else(|| panic!("no error at path {path}: {diags:?}"))
    }

    #[test]
    fn a_document_dataset_needs_a_non_empty_key_and_axes() {
        let (schema, diags) = cvi_with("key = [\"underlying_ref\"]", "key = []");
        assert!(schema.dataset("cvi_params").is_none());
        error_with_path(&diags, "cvi_params.key");
        let (schema, diags) = cvi_with("axes = [\"term\", \"node\"]", "axes = []");
        assert!(schema.dataset("cvi_params").is_none());
        error_with_path(&diags, "cvi_params.axes");
    }

    #[test]
    fn a_key_or_axis_naming_an_undeclared_column_drops_the_dataset() {
        let (schema, diags) = cvi_with("key = [\"underlying_ref\"]", "key = [\"nonesuch\"]");
        assert!(schema.dataset("cvi_params").is_none());
        assert!(error_with_path(&diags, "cvi_params.key").message.contains("nonesuch"));
        let (schema, diags) = cvi_with("axes = [\"term\", \"node\"]", "axes = [\"term\", \"nonesuch\"]");
        assert!(schema.dataset("cvi_params").is_none());
        assert!(error_with_path(&diags, "cvi_params.axes").message.contains("nonesuch"));
    }

    #[test]
    fn every_key_column_must_be_a_dimension() {
        // Make the key column an attribute: no longer scopeable, so no
        // longer a legal identity (spec §3.2).
        let (schema, diags) = cvi_with(
            "[cvi_params.columns.underlying_ref]\ntype = \"utf8\"\nrole = \"dimension\"",
            "[cvi_params.columns.underlying_ref]\ntype = \"utf8\"\nrole = \"attribute\"",
        );
        assert!(schema.dataset("cvi_params").is_none());
        error_with_path(&diags, "cvi_params.key");
    }

    #[test]
    fn axes_and_axis_roles_must_agree_both_ways() {
        // An axis column not listed in `axes`.
        let (schema, diags) = cvi_with("axes = [\"term\", \"node\"]", "axes = [\"term\"]");
        assert!(schema.dataset("cvi_params").is_none());
        assert!(error_with_path(&diags, "cvi_params.columns.node.role").message.contains("not listed in axes"));
        // A listed axis whose role is not `axis`.
        let (schema, diags) = cvi_with(
            "[cvi_params.columns.node]\ntype = \"f64\"\nrole = \"axis\"",
            "[cvi_params.columns.node]\ntype = \"f64\"\nrole = \"value\"",
        );
        assert!(schema.dataset("cvi_params").is_none());
        error_with_path(&diags, "cvi_params.axes");
    }

    #[test]
    fn a_non_numeric_value_column_is_dropped_and_the_dataset_kept() {
        let (schema, diags) = cvi_with(
            "[cvi_params.columns.param]\ntype = \"f64\"",
            "[cvi_params.columns.param]\ntype = \"utf8\"",
        );
        // Only value column gone → the "at least one value" rule fires
        // next, so add a second numeric value to isolate this rule.
        let _ = (schema, diags);
        let text = CVI
            .replace("[cvi_params.columns.param]\ntype = \"f64\"", "[cvi_params.columns.param]\ntype = \"utf8\"")
            + "\n[cvi_params.columns.param2]\ntype = \"i64\"\nrole = \"value\"\n";
        let (schema, diags) = SchemaSpec::from_doc(&doc(&text));
        let ds = schema.dataset("cvi_params").expect("dataset kept");
        assert!(ds.column("param").is_none(), "the utf8 value is dropped");
        assert!(ds.column("param2").is_some());
        error_with_path(&diags, "cvi_params.columns.param.type");
    }

    #[test]
    fn a_document_dataset_declares_at_least_one_value() {
        let (schema, diags) = cvi_with(
            "[cvi_params.columns.param]\ntype = \"f64\"\nrole = \"value\"",
            "[cvi_params.columns.param]\ntype = \"f64\"\nrole = \"attribute\"",
        );
        assert!(schema.dataset("cvi_params").is_none());
        error_with_path(&diags, "cvi_params.columns");
    }

    #[test]
    fn measure_vocabulary_on_a_document_dataset_is_refused_per_column() {
        let text = CVI.to_string()
            + "\n[cvi_params.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n"
            + "\n[cvi_params.columns.book]\ntype = \"utf8\"\nrole = \"key\"\n"
            + "\n[cvi_params.columns.ccy]\ntype = \"utf8\"\nrole = \"dimension\"\ngrain = \"instrument\"\n";
        let (schema, diags) = SchemaSpec::from_doc(&doc(&text));
        let ds = schema.dataset("cvi_params").expect("the dataset is kept; the columns are dropped");
        for name in ["npv", "book", "ccy"] {
            assert!(ds.column(name).is_none(), "{name} should be dropped");
            let d = error_with_path(&diags, &format!("cvi_params.columns.{name}.role"));
            assert!(d.message.contains("document family"), "{}", d.message);
        }
        assert!(ds.column("param").is_some(), "the legal columns survive");
    }

    #[test]
    fn document_vocabulary_on_a_measure_dataset_is_the_mirror_error() {
        let text = SAMPLE.to_string()
            + "\n[risk_snapshot.columns.tenor]\ntype = \"f64\"\nrole = \"axis\"\n"
            + "\n[risk_snapshot.columns.cell]\ntype = \"f64\"\nrole = \"value\"\n";
        let (schema, diags) = SchemaSpec::from_doc(&doc(&text));
        let ds = schema.dataset("risk_snapshot").unwrap();
        assert!(ds.column("tenor").is_none() && ds.column("cell").is_none());
        for name in ["tenor", "cell"] {
            let d = error_with_path(&diags, &format!("risk_snapshot.columns.{name}.role"));
            assert!(d.message.contains("measure family"), "{}", d.message);
        }
    }

    #[test]
    fn a_document_key_need_not_be_a_built_in_grain_key_column() {
        // `index_ref` is no grain's key column. On the measure family a
        // bare dimension of that name is dropped; on the document family
        // it is the whole point.
        let text = CVI
            .replace("key = [\"underlying_ref\"]", "key = [\"index_ref\"]")
            .replace("[cvi_params.columns.underlying_ref]", "[cvi_params.columns.index_ref]");
        let (schema, diags) = SchemaSpec::from_doc(&doc(&text));
        assert!(diags.is_empty(), "{diags:?}");
        let ds = schema.dataset("cvi_params").unwrap();
        assert!(ds.column("index_ref").is_some());
        assert!(ds.column("index_ref").unwrap().textual, "textual is routable through the key");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p geode-core schema::tests`
Expected: the new tests FAIL (the current validator either keeps the dataset or, for `index_ref`, drops it as a bare dimension outside every key).

- [ ] **Step 3: Implement `validate_document` and dispatch**

At the top of `validate_dataset`, after the reserved-column check (which applies to both families):

```rust
    if ds.is_document() {
        return validate_document(ds);
    }
```

and make the measure-family body refuse the document vocabulary before its grain checks:

```rust
    // Document vocabulary on a measure dataset (market-data spec §3.2):
    // refused per column, never guessed at. The document family has the
    // mirror rule in `validate_document`.
    let foreign: Vec<String> = ds
        .columns
        .iter()
        .filter(|c| matches!(c.role, ColumnRole::Axis | ColumnRole::Value | ColumnRole::Attribute { grain: None }))
        .map(|c| c.name.clone())
        .collect();
    for name in &foreign {
        diags.push(Diagnostic {
            severity: Severity::Error,
            layer: None,
            file: None,
            message: format!(
                "dataset '{}' column '{name}': 'axis', 'value' and a grainless 'attribute' belong to the \
                 document family; this is a measure-family dataset — column dropped",
                ds.name
            ),
            path: Some(format!("{}.columns.{name}.role", ds.name)),
        });
    }
    ds.columns.retain(|c| !foreign.contains(&c.name));
```

(`Attribute { grain: None }` cannot actually be produced by `parse_column` for the measure family — it errors on the missing grain — but the retain keeps the invariant local to this function rather than relying on the parser.)

Then:

```rust
/// The document family's load-time rules (market-data spec §3.2). Every
/// failure is a diagnostic with `path` set; `false` means the dataset is
/// dropped, `true` that it is kept with the offending columns removed.
fn validate_document(ds: &mut DatasetSpec) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let err = |message: String, path: String| Diagnostic {
        severity: Severity::Error,
        layer: None,
        file: None,
        message,
        path: Some(path),
    };
    let name = ds.name.clone();

    // Measure vocabulary is refused per column, never reinterpreted.
    let foreign: Vec<String> = ds
        .columns
        .iter()
        .filter(|c| matches!(c.role, ColumnRole::Key | ColumnRole::Measure { .. } | ColumnRole::Dimension { grain: Some(_) } | ColumnRole::Attribute { grain: Some(_) }))
        .map(|c| c.name.clone())
        .collect();
    for c in &foreign {
        diags.push(err(
            format!("dataset '{name}' column '{c}': 'key', 'measure' and 'grain = …' belong to the measure family; this is a document-family dataset — column dropped"),
            format!("{name}.columns.{c}.role"),
        ));
    }
    ds.columns.retain(|c| !foreign.contains(&c.name));

    // A value is a number.
    let non_numeric: Vec<String> = ds
        .columns
        .iter()
        .filter(|c| c.role == ColumnRole::Value && !matches!(c.ty, ColumnType::F64 | ColumnType::I64))
        .map(|c| c.name.clone())
        .collect();
    for c in &non_numeric {
        diags.push(err(
            format!("dataset '{name}' column '{c}': a value must be f64 or i64 — column dropped"),
            format!("{name}.columns.{c}.type"),
        ));
    }
    ds.columns.retain(|c| !non_numeric.contains(&c.name));

    let mut keep = true;

    if ds.key.is_empty() {
        diags.push(err(format!("dataset '{name}': a document dataset needs a non-empty 'key'; dataset dropped"), format!("{name}.key")));
        keep = false;
    }
    if ds.axes.is_empty() {
        diags.push(err(format!("dataset '{name}': a document dataset needs non-empty 'axes'; dataset dropped"), format!("{name}.axes")));
        keep = false;
    }
    for k in &ds.key {
        match ds.column(k) {
            None => {
                diags.push(err(format!("dataset '{name}': key names undeclared column '{k}'; dataset dropped"), format!("{name}.key")));
                keep = false;
            }
            Some(c) if !matches!(c.role, ColumnRole::Dimension { grain: None }) => {
                diags.push(err(format!("dataset '{name}': key column '{k}' must have role = \"dimension\"; dataset dropped"), format!("{name}.key")));
                keep = false;
            }
            Some(_) => {}
        }
    }
    for a in &ds.axes {
        match ds.column(a) {
            None => {
                diags.push(err(format!("dataset '{name}': axes names undeclared column '{a}'; dataset dropped"), format!("{name}.axes")));
                keep = false;
            }
            Some(c) if c.role != ColumnRole::Axis => {
                diags.push(err(format!("dataset '{name}': axis '{a}' must have role = \"axis\"; dataset dropped"), format!("{name}.axes")));
                keep = false;
            }
            Some(_) => {}
        }
    }
    for c in ds.columns.iter().filter(|c| c.role == ColumnRole::Axis && !ds.axes.contains(&c.name)) {
        diags.push(err(
            format!("dataset '{name}' column '{}': role = \"axis\" but not listed in axes; dataset dropped", c.name),
            format!("{name}.columns.{}.role", c.name),
        ));
        keep = false;
    }
    if !ds.columns.iter().any(|c| c.role == ColumnRole::Value) {
        diags.push(err(format!("dataset '{name}': a document dataset declares at least one value column; dataset dropped"), format!("{name}.columns")));
        keep = false;
    }

    // `textual` is routable through any dimension of a document dataset
    // (there is no grain to route it): only a textual axis/value/attribute
    // is refused, and it is cleared, not dropped, as on the measure side.
    let unroutable: Vec<String> = ds
        .columns
        .iter()
        .filter(|c| c.textual && !matches!(c.role, ColumnRole::Dimension { .. }))
        .map(|c| c.name.clone())
        .collect();
    for c in &unroutable {
        diags.push(err(
            format!("dataset '{name}' column '{c}': textual = true on a non-dimension of a document dataset; textual ignored"),
            format!("{name}.columns.{c}.textual"),
        ));
    }
    for c in &mut ds.columns {
        if unroutable.contains(&c.name) {
            c.textual = false;
        }
    }

    if !keep {
        ds.columns.clear();
        ds.key.clear();
        ds.axes.clear();
    }
    diags
}
```

`from_doc` must drop a document dataset whose validation said so. Rather than threading a `bool`, use the emptied shape: after `diags.extend(validate_dataset(&mut dataset));` add

```rust
            if dataset.is_document() && dataset.columns.is_empty() {
                // `validate_document` empties a dataset it refused — never
                // push a document dataset with no columns, it could not
                // be stored or queried.
                continue;
            }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p geode-core schema::tests` then `cargo test --workspace`
Expected: PASS.

- [ ] **Step 5: Add mutation entries**

Append after the last `run_mutation` in `scripts/mutation-check.sh` (confirm each anchor occurs exactly once with `grep -c -F`):

```zsh
run_mutation "schema/document: an empty key drops the dataset" \
  crates/geode-core/src/schema/mod.rs \
  '    if ds.key.is_empty() {' \
  '    if false {' \
  geode-core a_document_dataset_needs_a_non_empty_key_and_axes

run_mutation "schema/document: a key column must be a dimension" \
  crates/geode-core/src/schema/mod.rs \
  '            Some(c) if !matches!(c.role, ColumnRole::Dimension { grain: None }) => {' \
  '            Some(c) if false && !matches!(c.role, ColumnRole::Dimension { grain: None }) => {' \
  geode-core every_key_column_must_be_a_dimension

run_mutation "schema/document: an axis role not listed in axes is refused" \
  crates/geode-core/src/schema/mod.rs \
  '    for c in ds.columns.iter().filter(|c| c.role == ColumnRole::Axis && !ds.axes.contains(&c.name)) {' \
  '    for c in ds.columns.iter().filter(|c| false && c.role == ColumnRole::Axis && !ds.axes.contains(&c.name)) {' \
  geode-core axes_and_axis_roles_must_agree_both_ways

run_mutation "schema/document: measure vocabulary is dropped per column" \
  crates/geode-core/src/schema/mod.rs \
  '    ds.columns.retain(|c| !foreign.contains(&c.name));
    let mut keep = true;' \
  '    let _ = &foreign;
    let mut keep = true;' \
  geode-core measure_vocabulary_on_a_document_dataset_is_refused_per_column

run_mutation "schema/measures: document vocabulary is dropped per column" \
  crates/geode-core/src/schema/mod.rs \
  '    ds.columns.retain(|c| !foreign.contains(&c.name));

    // Every grain in use groups by its key columns' \
  '    let _ = &foreign;

    // Every grain in use groups by its key columns' \
  geode-core document_vocabulary_on_a_measure_dataset_is_the_mirror_error

run_mutation "schema/document: a refused dataset is not pushed" \
  crates/geode-core/src/schema/mod.rs \
  '            if dataset.is_document() && dataset.columns.is_empty() {' \
  '            if false {' \
  geode-core a_document_dataset_declares_at_least_one_value
```

The two `retain` anchors are multi-line so each matches once; the second one must include the comment line that follows it in the measure body — adjust the anchor's trailing line to whatever comment actually follows in your edit, then re-check `grep -c`.

Run: `zsh scripts/mutation-check.sh --anchors-only` → exit 0; then commit; then `nohup zsh scripts/mutation-check.sh "schema/" > /tmp/mut.log 2>&1 &` and confirm every new entry prints `caught`.

- [ ] **Step 6: Commit**

```bash
git add crates/geode-core/src/schema/mod.rs scripts/mutation-check.sh
git commit -m "schema: validate the document family — key, axes, values, mirror refusals, textual routing"
```

---

### Task 3: What the rest of the schema sees

**Files:**
- Modify: `crates/geode-core/src/schema/mod.rs` (`grains`, `groupable_columns`, `categorical_columns`, new `document_columns`)
- Test: `crates/geode-core/src/schema/mod.rs` `mod tests`
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Produces: `DatasetSpec::document_columns(&self) -> Vec<&ColumnSpec>` — key dimensions in `key` order, then axes in `axes` order, then values in schema order, then attributes in schema order; **this is the storage column order and the document request's projection order** (Tasks 6 and 8 both read it). `groupable_columns` on a document dataset returns its `Dimension` columns (in schema order) and never an axis. `grains()` is empty for a document dataset (already true: no column has a grain).

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn a_document_dataset_has_no_grain_and_groups_by_its_dimensions_only() {
        let (schema, _) = SchemaSpec::from_doc(&doc(CVI));
        let ds = schema.dataset("cvi_params").unwrap();
        assert!(ds.grains().is_empty());
        assert_eq!(ds.groupable_columns(), vec!["underlying_ref"]);
        assert_eq!(ds.categorical_columns(), vec!["underlying_ref"], "a utf8 dimension defaults to categorical here too");
    }

    #[test]
    fn document_columns_are_key_then_axes_then_values_then_attributes() {
        let (schema, _) = SchemaSpec::from_doc(&doc(CVI));
        let ds = schema.dataset("cvi_params").unwrap();
        let names: Vec<&str> = ds.document_columns().iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["underlying_ref", "term", "node", "param", "anchor_date", "spot_ref"]);
        // Order comes from `key`/`axes`, not from the TOML: swap the axes.
        let (schema, _) = cvi_with("axes = [\"term\", \"node\"]", "axes = [\"node\", \"term\"]");
        let ds = schema.dataset("cvi_params").unwrap();
        let names: Vec<&str> = ds.document_columns().iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names[1..3], ["node", "term"]);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p geode-core schema::tests -- a_document_dataset_has_no_grain document_columns_are`
Expected: `groupable_columns` returns empty for a document dataset (no grains → nothing carried) and `document_columns` does not exist.

- [ ] **Step 3: Implement**

```rust
    pub fn groupable_columns(&self) -> Vec<&str> {
        if self.is_document() {
            // No grain carries anything here; the identity dimensions are
            // the whole grouping vocabulary and an axis is row identity
            // *within* a document, never something the frame groups by
            // (market-data spec §3.3).
            return self
                .columns
                .iter()
                .filter(|c| matches!(c.role, ColumnRole::Dimension { .. }))
                .map(|c| c.name.as_str())
                .collect();
        }
        // …existing body…
    }

    /// The document family's storage and projection order (market-data
    /// spec §3.1, §7): key in declared order, axes in declared order,
    /// then values and attributes in schema order. `ddl::
    /// create_document_table_sql` and the document request both read
    /// this, so the two can never disagree about column positions.
    pub fn document_columns(&self) -> Vec<&ColumnSpec> {
        let mut out: Vec<&ColumnSpec> = Vec::with_capacity(self.columns.len());
        out.extend(self.key.iter().filter_map(|k| self.column(k)));
        out.extend(self.axes.iter().filter_map(|a| self.column(a)));
        out.extend(self.columns.iter().filter(|c| c.role == ColumnRole::Value));
        out.extend(self.columns.iter().filter(|c| matches!(c.role, ColumnRole::Attribute { grain: None })));
        out
    }
```

Update `groupable_columns`'s doc comment: it now describes both families.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p geode-core` → PASS.

- [ ] **Step 5: Mutation entries and commit**

```zsh
run_mutation "schema/document: groupable columns are the dimensions, not the axes" \
  crates/geode-core/src/schema/mod.rs \
  '                .filter(|c| matches!(c.role, ColumnRole::Dimension { .. }))
                .map(|c| c.name.as_str())
                .collect();
        }' \
  '                .filter(|c| matches!(c.role, ColumnRole::Dimension { .. } | ColumnRole::Axis))
                .map(|c| c.name.as_str())
                .collect();
        }' \
  geode-core a_document_dataset_has_no_grain_and_groups_by_its_dimensions_only

run_mutation "schema/document: document_columns follows the declared axes order" \
  crates/geode-core/src/schema/mod.rs \
  '        out.extend(self.axes.iter().filter_map(|a| self.column(a)));' \
  '        out.extend(self.columns.iter().filter(|c| c.role == ColumnRole::Axis));' \
  geode-core document_columns_are_key_then_axes_then_values_then_attributes
```

```bash
git add crates/geode-core/src/schema/mod.rs scripts/mutation-check.sh
git commit -m "schema: document_columns order and the document family's grouping vocabulary"
```

---

### Task 4: `geode_core::document` — the rows a document becomes

**Files:**
- Create: `crates/geode-core/src/document.rs`
- Modify: `crates/geode-core/src/lib.rs` (`pub mod document;`)
- Test: `crates/geode-core/src/document.rs` `mod tests`

**Interfaces:**
- Produces:

```rust
pub const KEY_SEPARATOR: char = '\u{1f}';
pub fn join_key(parts: &[String]) -> String;           // parts joined by KEY_SEPARATOR
pub fn split_key(batch: &str) -> Vec<String>;          // the inverse; a batch with no separator is one part

#[derive(Debug, Clone, PartialEq)]
pub enum Column { F64(Vec<f64>), I64(Vec<i64>), Utf8(Vec<String>), Date(Vec<chrono::NaiveDate>) }
impl Column { pub fn len(&self) -> usize; pub fn is_empty(&self) -> bool; pub fn column_type(&self) -> ColumnType; }

#[derive(Debug, Clone, PartialEq)]
pub enum Value { F64(f64), I64(i64), Utf8(String), Date(chrono::NaiveDate) }
impl Value { pub fn column_type(&self) -> ColumnType; }

/// One parsed document, struct-of-arrays (market-data spec §6.2).
#[derive(Debug, Clone, PartialEq)]
pub struct DocumentRows {
    pub key: Vec<String>,                  // in the dataset's `key` order
    pub attributes: Vec<(String, Value)>,  // document-level, one value each
    pub axes: Vec<(String, Column)>,       // one per axis, all the same length
    pub values: Vec<(String, Column)>,     // one per value column, same length
}
impl DocumentRows {
    pub fn rows(&self) -> usize;           // length of the first axis column, 0 if none
    /// Every check `publish_document` needs before touching the store:
    /// key arity, axis names/order/types, value names/types, attribute
    /// names/types against `document_columns()`, and equal lengths.
    pub fn validate(&self, ds: &DatasetSpec) -> Result<(), String>;
}
```

Note `KEY_SEPARATOR` is a `char`; `join_key` uses it as the separator and `split_key` splits on it. A dimension value containing it is refused by `validate` ("key part contains the reserved separator").

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LayerDoc, merge_docs};
    use crate::schema::SchemaSpec;
    use chrono::NaiveDate;

    const CVI: &str = r#"
[cvi_params]
family = "document"
key = ["underlying_ref"]
axes = ["term", "node"]
[cvi_params.columns.underlying_ref]
type = "utf8"
role = "dimension"
[cvi_params.columns.term]
type = "date"
role = "axis"
[cvi_params.columns.node]
type = "f64"
role = "axis"
[cvi_params.columns.param]
type = "f64"
role = "value"
[cvi_params.columns.anchor_date]
type = "date"
role = "attribute"
[cvi_params.columns.spot_ref]
type = "f64"
role = "attribute"
"#;

    fn cvi() -> crate::schema::DatasetSpec {
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", CVI).unwrap()]);
        let (schema, diags) = SchemaSpec::from_doc(&doc);
        assert!(diags.is_empty(), "{diags:?}");
        schema.dataset("cvi_params").unwrap().clone()
    }

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    /// Two terms × three nodes, term-major.
    pub(crate) fn sample() -> DocumentRows {
        DocumentRows {
            key: vec!["SPX.Z".into()],
            attributes: vec![
                ("anchor_date".into(), Value::Date(d("2026-09-12"))),
                ("spot_ref".into(), Value::F64(7650.0)),
            ],
            axes: vec![
                ("term".into(), Column::Date(vec![d("2026-09-18"); 3].into_iter().chain(vec![d("2026-10-16"); 3]).collect())),
                ("node".into(), Column::F64(vec![-20.0, -1.0, 3.5, -20.0, -1.0, 3.5])),
            ],
            values: vec![("param".into(), Column::F64(vec![-0.34, 0.1, 1.3, -0.3, 0.12, 1.25]))],
        }
    }

    #[test]
    fn join_and_split_round_trip_and_a_single_part_needs_no_separator() {
        let parts = vec!["SPX.Z".to_string(), "NDX.Z".to_string()];
        let joined = join_key(&parts);
        assert!(joined.contains(KEY_SEPARATOR));
        assert_eq!(split_key(&joined), parts);
        assert_eq!(join_key(&["SPX.Z".to_string()]), "SPX.Z");
        assert_eq!(split_key("SPX.Z"), vec!["SPX.Z".to_string()]);
    }

    #[test]
    fn a_well_formed_document_validates_against_its_dataset() {
        assert_eq!(sample().validate(&cvi()), Ok(()));
        assert_eq!(sample().rows(), 6);
    }

    #[test]
    fn validate_refuses_each_mismatch_with_a_message_naming_it() {
        let ds = cvi();
        let mut r = sample();
        r.key.push("extra".into());
        assert!(r.validate(&ds).unwrap_err().contains("key has 2 parts, dataset declares 1"));

        let mut r = sample();
        r.axes.swap(0, 1);
        assert!(r.validate(&ds).unwrap_err().contains("axis 0 is 'node', dataset declares 'term'"));

        let mut r = sample();
        r.axes[1].1 = Column::Utf8(vec!["x".into(); 6]);
        assert!(r.validate(&ds).unwrap_err().contains("axis 'node' is utf8, dataset declares f64"));

        let mut r = sample();
        r.values[0].1 = Column::F64(vec![1.0; 5]);
        assert!(r.validate(&ds).unwrap_err().contains("value 'param' has 5 rows, axes have 6"));

        let mut r = sample();
        r.values[0].0 = "nonesuch".into();
        assert!(r.validate(&ds).unwrap_err().contains("value 'nonesuch' is not declared"));

        let mut r = sample();
        r.attributes.retain(|(n, _)| n != "spot_ref");
        assert!(r.validate(&ds).unwrap_err().contains("attribute 'spot_ref' is missing"));

        let mut r = sample();
        r.attributes[1].1 = Value::Utf8("7650".into());
        assert!(r.validate(&ds).unwrap_err().contains("attribute 'spot_ref' is utf8, dataset declares f64"));

        let mut r = sample();
        r.key[0] = format!("SPX{KEY_SEPARATOR}Z");
        assert!(r.validate(&ds).unwrap_err().contains("reserved separator"));
    }

    #[test]
    fn validate_refuses_a_measure_dataset() {
        let mut ds = cvi();
        ds.family = crate::schema::Family::Measures;
        assert!(sample().validate(&ds).unwrap_err().contains("not a document dataset"));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p geode-core document::` → compile error, module missing.

- [ ] **Step 3: Implement `document.rs`**

```rust
//! The rows a parsed document becomes (market-data spec §6.2). Lives in
//! `geode-core` because both the data crate (which publishes them) and
//! the demo generator (which produces them) need the type, and the data
//! crate dev-depends on the generator — a trait or type in either would
//! be a cycle. Struct-of-arrays, per PHILOSOPHY §6: nothing here is a row.

use crate::schema::{ColumnRole, ColumnType, DatasetSpec};
use chrono::NaiveDate;

/// Joins the parts of a multi-column document key into the one string the
/// store's `batch` column holds. ASCII unit separator: no dimension value
/// may contain it (`DocumentRows::validate` refuses one that does), so the
/// join is unambiguous and `split_key` is its exact inverse.
pub const KEY_SEPARATOR: char = '\u{1f}';

pub fn join_key(parts: &[String]) -> String {
    parts.join(&KEY_SEPARATOR.to_string())
}

pub fn split_key(batch: &str) -> Vec<String> {
    batch.split(KEY_SEPARATOR).map(str::to_string).collect()
}

#[derive(Debug, Clone, PartialEq)]
pub enum Column {
    F64(Vec<f64>),
    I64(Vec<i64>),
    Utf8(Vec<String>),
    Date(Vec<NaiveDate>),
}

impl Column {
    pub fn len(&self) -> usize {
        match self {
            Column::F64(v) => v.len(),
            Column::I64(v) => v.len(),
            Column::Utf8(v) => v.len(),
            Column::Date(v) => v.len(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn column_type(&self) -> ColumnType {
        match self {
            Column::F64(_) => ColumnType::F64,
            Column::I64(_) => ColumnType::I64,
            Column::Utf8(_) => ColumnType::Utf8,
            Column::Date(_) => ColumnType::Date,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    F64(f64),
    I64(i64),
    Utf8(String),
    Date(NaiveDate),
}

impl Value {
    pub fn column_type(&self) -> ColumnType {
        match self {
            Value::F64(_) => ColumnType::F64,
            Value::I64(_) => ColumnType::I64,
            Value::Utf8(_) => ColumnType::Utf8,
            Value::Date(_) => ColumnType::Date,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DocumentRows {
    pub key: Vec<String>,
    pub attributes: Vec<(String, Value)>,
    pub axes: Vec<(String, Column)>,
    pub values: Vec<(String, Column)>,
}

fn type_name(t: ColumnType) -> &'static str {
    match t {
        ColumnType::Utf8 => "utf8",
        ColumnType::F64 => "f64",
        ColumnType::I64 => "i64",
        ColumnType::Date => "date",
        ColumnType::Timestamp => "timestamp",
        ColumnType::Bool => "bool",
    }
}

impl DocumentRows {
    pub fn rows(&self) -> usize {
        self.axes.first().map_or(0, |(_, c)| c.len())
    }

    /// Everything `publish_document` must know is true before it stages a
    /// row. Names and order come from `DatasetSpec::document_columns`, so
    /// a document that validates here lands in the table in the right
    /// positions by construction.
    pub fn validate(&self, ds: &DatasetSpec) -> Result<(), String> {
        if !ds.is_document() {
            return Err(format!("dataset '{}' is not a document dataset", ds.name));
        }
        if self.key.len() != ds.key.len() {
            return Err(format!("key has {} parts, dataset declares {}", self.key.len(), ds.key.len()));
        }
        if let Some(part) = self.key.iter().find(|p| p.contains(KEY_SEPARATOR)) {
            return Err(format!("key part {part:?} contains the reserved separator"));
        }
        if self.axes.len() != ds.axes.len() {
            return Err(format!("document has {} axes, dataset declares {}", self.axes.len(), ds.axes.len()));
        }
        for (i, ((name, col), declared)) in self.axes.iter().zip(&ds.axes).enumerate() {
            if name != declared {
                return Err(format!("axis {i} is '{name}', dataset declares '{declared}'"));
            }
            let spec = ds.column(declared).expect("validated by the schema");
            if col.column_type() != spec.ty {
                return Err(format!("axis '{name}' is {}, dataset declares {}", type_name(col.column_type()), type_name(spec.ty)));
            }
        }
        let rows = self.rows();
        for (name, col) in &self.axes {
            if col.len() != rows {
                return Err(format!("axis '{name}' has {} rows, first axis has {rows}", col.len()));
            }
        }
        for (name, col) in &self.values {
            let Some(spec) = ds.column(name).filter(|c| c.role == ColumnRole::Value) else {
                return Err(format!("value '{name}' is not declared"));
            };
            if col.column_type() != spec.ty {
                return Err(format!("value '{name}' is {}, dataset declares {}", type_name(col.column_type()), type_name(spec.ty)));
            }
            if col.len() != rows {
                return Err(format!("value '{name}' has {} rows, axes have {rows}", col.len()));
            }
        }
        for spec in ds.columns.iter().filter(|c| c.role == ColumnRole::Value) {
            if !self.values.iter().any(|(n, _)| n == &spec.name) {
                return Err(format!("value '{}' is missing", spec.name));
            }
        }
        for spec in ds.columns.iter().filter(|c| matches!(c.role, ColumnRole::Attribute { grain: None })) {
            let Some((_, v)) = self.attributes.iter().find(|(n, _)| n == &spec.name) else {
                return Err(format!("attribute '{}' is missing", spec.name));
            };
            if v.column_type() != spec.ty {
                return Err(format!("attribute '{}' is {}, dataset declares {}", spec.name, type_name(v.column_type()), type_name(spec.ty)));
            }
        }
        for (name, _) in &self.attributes {
            if !ds.columns.iter().any(|c| &c.name == name && matches!(c.role, ColumnRole::Attribute { grain: None })) {
                return Err(format!("attribute '{name}' is not declared"));
            }
        }
        Ok(())
    }
}
```

Add `pub mod document;` to `lib.rs`. Make `tests::sample()` `pub(crate)` only if another core test needs it; Task 7's `geode-data` tests build their own copy (a dev-dependency cannot reach a `#[cfg(test)]` item).

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p geode-core document::` → PASS. `cargo clippy --workspace --all-targets -- -D warnings` → clean.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-core/src/document.rs crates/geode-core/src/lib.rs
git commit -m "core: DocumentRows, the struct-of-arrays shape a parsed document publishes as"
```

---

### Task 5: `ScopeSemantics::NotApplicable` and `Scope::applicable_to`

**Files:**
- Modify: `crates/geode-core/src/attribution.rs`
- Modify: `crates/geode-core/src/scope/mod.rs`
- Test: both files' `mod tests`
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Produces: `ScopeSemantics::NotApplicable { dimensions: Vec<String> }`; `ScopeSemantics::meet` treats it as weaker than `SemiJoined` (union of dimensions); `Scope::applicable_to(&self, ds: &DatasetSpec, dims: &DerivedDimensions) -> (Scope, Vec<String>)` — the scope with every dimension selection the dataset (or a derived dimension over one of its columns) lacks removed, plus the removed column names in scope order. `text` and `expression` pass through untouched (an expression naming an unknown column is already a validation error at the point of entry).

- [ ] **Step 1: Write the failing tests**

In `attribution.rs`:

```rust
    #[test]
    fn not_applicable_is_weaker_than_semi_joined_and_unions_dimensions() {
        let na = ScopeSemantics::NotApplicable { dimensions: vec!["book".into()] };
        let sj = ScopeSemantics::SemiJoined { dimensions: vec!["underlying_ref".into()] };
        assert_eq!(
            na.clone().meet(sj.clone()),
            ScopeSemantics::NotApplicable { dimensions: vec!["book".into(), "underlying_ref".into()] }
        );
        assert_eq!(sj.meet(na.clone()), ScopeSemantics::NotApplicable { dimensions: vec!["underlying_ref".into(), "book".into()] });
        assert_eq!(ScopeSemantics::Direct.meet(na.clone()), na);
        assert!(!na.is_direct());
    }
```

In `scope/mod.rs` tests (it has a dataset fixture helper — reuse whatever `Scope::validate`'s tests use to build a `DatasetSpec`; if none, build one with `DatasetSpec { name: "cvi".into(), columns: vec![dim("underlying_ref")], family: Family::Document, key: vec!["underlying_ref".into()], axes: vec![] , }` using the local `ColumnSpec` helper pattern already in that test module):

```rust
    #[test]
    fn applicable_to_drops_selections_on_columns_the_dataset_lacks_and_names_them() {
        let ds = document_dataset_with_dimension("underlying_ref");
        let scope = Scope {
            dimensions: vec![
                DimensionSelection { column: "book".into(), values: vec!["EQD".into()] },
                DimensionSelection { column: "underlying_ref".into(), values: vec!["SPX.Z".into()] },
                DimensionSelection { column: "lhu".into(), values: vec!["A".into()] },
            ],
            text: Some("spx".into()),
            expression: None,
            impossible: false,
        };
        let (kept, dropped) = scope.applicable_to(&ds, &DerivedDimensions::default());
        assert_eq!(dropped, vec!["book".to_string(), "lhu".to_string()]);
        assert_eq!(kept.dimensions.len(), 1);
        assert_eq!(kept.dimensions[0].column, "underlying_ref");
        assert_eq!(kept.text.as_deref(), Some("spx"), "text passes through");
        // A derived dimension over a column the dataset has is kept.
        let mut dims = DerivedDimensions::default();
        dims.insert_for_tests("region", "underlying_ref"); // use the crate's existing test constructor for a derived dimension
        let scope = Scope { dimensions: vec![DimensionSelection { column: "region".into(), values: vec!["US".into()] }], ..Scope::default() };
        let (kept, dropped) = scope.applicable_to(&ds, &dims);
        assert!(dropped.is_empty());
        assert_eq!(kept.dimensions.len(), 1);
    }
```

If `DerivedDimensions` has no test constructor, build one through its `from_doc` with a two-line TOML fixture the way `dimensions.rs`'s own tests do; the assertion is what matters.

- [ ] **Step 2: Run to verify failure** — `cargo test -p geode-core attribution scope::` → compile errors.

- [ ] **Step 3: Implement**

`attribution.rs`:

```rust
pub enum ScopeSemantics {
    Direct,
    SemiJoined { dimensions: Vec<String> },
    /// Some scope selection named a dimension this dataset does not have
    /// at all, so it was dropped for this query rather than applied
    /// (market-data spec §3.4). Weaker than `SemiJoined`: a semi-join
    /// still narrowed the rows, a dropped selection did not.
    NotApplicable { dimensions: Vec<String> },
}
```

and `meet`: `NotApplicable` wins over anything, with the union of the dimensions of both sides (its own first, then the other's `SemiJoined`/`NotApplicable` dimensions not already present); `SemiJoined` wins over `Direct` as before. Every exhaustive `match` on `ScopeSemantics` in the workspace (the compiler will list them: `scope_sql.rs`, blotter `delegate.rs`'s marker painting if it matches rather than calls `is_direct`) gets a `NotApplicable` arm — in the blotter, paint it with the same marker `SemiJoined` gets; no new style in this part.

`scope/mod.rs`:

```rust
    /// The scope as it applies to one dataset: dimension selections on
    /// columns the dataset lacks — resolving a derived dimension to the
    /// column it derives from — are removed and returned by name, so the
    /// query drops them and the snapshot's provenance can say so
    /// (`ScopeSemantics::NotApplicable`, market-data spec §3.4). Text and
    /// expression pass through: the text filter already routes by the
    /// dataset's own textual columns, and an expression naming an
    /// unknown column is refused at the point of entry by `validate`.
    pub fn applicable_to(&self, ds: &DatasetSpec, dims: &DerivedDimensions) -> (Scope, Vec<String>) {
        let mut kept = self.clone();
        let mut dropped = Vec::new();
        kept.dimensions.retain(|d| {
            let present = match dims.get(&d.column) {
                Some(derived) => ds.column(&derived.from).is_some(),
                None => ds.column(&d.column).is_some(),
            };
            if !present {
                dropped.push(d.column.clone());
            }
            present
        });
        (kept, dropped)
    }
```

- [ ] **Step 4: Run to verify pass** — `cargo test --workspace` → PASS; clippy clean.

- [ ] **Step 5: Mutation entries and commit**

```zsh
run_mutation "scope: applicable_to drops a selection on a column the dataset lacks" \
  crates/geode-core/src/scope/mod.rs \
  '            if !present {
                dropped.push(d.column.clone());
            }
            present' \
  '            if !present {
                dropped.push(d.column.clone());
            }
            true' \
  geode-core applicable_to_drops_selections_on_columns_the_dataset_lacks_and_names_them

run_mutation "attribution: NotApplicable is weaker than SemiJoined" \
  crates/geode-core/src/attribution.rs \
  '<the first line of the NotApplicable arm in meet — copy it verbatim once written>' \
  '<the same line with the arm made to return the SemiJoined side>' \
  geode-core not_applicable_is_weaker_than_semi_joined_and_unions_dimensions
```

Fill the second entry's anchors from the code as written; confirm with `grep -c -F`.

```bash
git add crates/geode-core/src/attribution.rs crates/geode-core/src/scope/mod.rs crates/geode-blotter scripts/mutation-check.sh
git commit -m "core: ScopeSemantics::NotApplicable and Scope::applicable_to"
```

---

### Task 6: `TablePair` — one table vocabulary for both families in `store`

This is the refactor that lets `publish_file`, `retention::sweep`, `history_of` and `apply_schema` serve document tables. It changes no behaviour for measure datasets; every existing store test must pass unchanged in meaning (some will need their calls rewritten).

**Files:**
- Modify: `crates/geode-data/src/store/ddl.rs`
- Modify: `crates/geode-data/src/store/mod.rs` (`apply_schema`)
- Modify: `crates/geode-data/src/store/publish.rs` (`PublishRequest.grain` → `tables: TablePair`)
- Modify: `crates/geode-data/src/store/retention.rs` (`sweep(.., grains: &[Grain], ..)` → `sweep(.., pairs: &[TablePair], ..)`)
- Modify: `crates/geode-data/src/ingest/load.rs` (the `publish_file` call site, line ~227)
- Modify: `crates/geode-data/src/service.rs` (any `sweep`/`history_of` call sites — grep `sweep(` and `history_of(`)
- Modify: `crates/geode-data/src/query/compile.rs:1062`, `crates/geode-data/src/query/distinct.rs:356` (`history_of` callers — unchanged signature, but verify)
- Test: `ddl.rs`, `publish.rs`, `retention.rs` test modules
- Modify: `scripts/mutation-check.sh` (existing anchors in these files may move — run `--anchors-only` after every edit and re-anchor any it reports)

**Interfaces:**
- Produces:

```rust
// ddl.rs
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TablePair { pub live: String, pub archive: String }
impl TablePair {
    pub fn for_grain(dataset: &str, grain: Grain) -> TablePair;   // table_name(..) for each kind
    pub fn for_document(dataset: &str) -> TablePair;               // "{dataset}_document_live" / "_archive"
    pub fn of(&self, kind: TableKind) -> &str;
}
/// Every live/archive pair a dataset owns: one per grain for the measure
/// family, exactly one for the document family.
pub fn table_pairs(ds: &DatasetSpec) -> Vec<TablePair>;
pub fn create_document_table_sql(ds: &DatasetSpec, kind: TableKind) -> String;
pub fn history_of(dataset: &str, ds: &DatasetSpec) -> Vec<String>;   // now = table_pairs(ds) flattened; signature unchanged
// publish.rs
pub struct PublishRequest { pub dataset: String, pub tables: TablePair, pub staging_table: String, pub partitions: Vec<Partition>, pub gen_id: i64, pub source_time: DateTime<Utc>, pub live_source_time: Option<DateTime<Utc>> }
// retention.rs
pub fn sweep(conn, ds: &DatasetSpec, pairs: &[TablePair], policy: &RetentionPolicy, now: DateTime<Utc>) -> Result<SweepReport, StoreError>
```

- [ ] **Step 1: Write the failing DDL tests**

In `ddl.rs` tests (reuse the module's existing `DatasetSpec` fixture builder for measure datasets; for the document dataset, parse the `CVI` TOML from Task 1 through `SchemaSpec::from_doc` — copy the constant into this test module):

```rust
    #[test]
    fn a_document_dataset_has_one_pair_named_document_and_no_grain_pairs() {
        let ds = cvi_dataset();
        let pairs = table_pairs(&ds);
        assert_eq!(pairs, vec![TablePair { live: "cvi_params_document_live".into(), archive: "cvi_params_document_archive".into() }]);
        assert_eq!(TablePair::for_document("cvi_params"), pairs[0]);
        assert_eq!(history_of("cvi_params", &ds), vec!["cvi_params_document_archive".to_string(), "cvi_params_document_live".to_string()]);
    }

    #[test]
    fn a_measure_dataset_has_one_pair_per_grain_in_grain_order() {
        let ds = risk_snapshot_fixture(); // the module's existing measure fixture
        let pairs = table_pairs(&ds);
        assert_eq!(pairs.len(), ds.grains().len());
        for (pair, grain) in pairs.iter().zip(ds.grains()) {
            assert_eq!(*pair, TablePair::for_grain("risk_snapshot", grain));
        }
    }

    #[test]
    fn document_table_columns_are_document_columns_then_the_storage_columns() {
        let ds = cvi_dataset();
        let sql = create_document_table_sql(&ds, TableKind::Live);
        assert!(sql.starts_with("CREATE TABLE IF NOT EXISTS cvi_params_document_live ("), "{sql}");
        let expected = [
            "\"underlying_ref\" VARCHAR", "\"term\" DATE", "\"node\" DOUBLE", "\"param\" DOUBLE",
            "\"anchor_date\" DATE", "\"spot_ref\" DOUBLE",
            "\"batch\" VARCHAR", "\"source_file_id\" BIGINT", "\"gen_id\" BIGINT", "\"source_time\" TIMESTAMP WITH TIME ZONE",
        ];
        let mut last = 0;
        for col in expected {
            let at = sql[last..].find(col).unwrap_or_else(|| panic!("{col} missing or out of order in {sql}"));
            last += at + col.len();
        }
        assert_eq!(
            create_document_table_sql(&ds, TableKind::Archive).replace("_archive", "_live"),
            sql,
            "live and archive carry identical columns"
        );
    }

    #[test]
    fn apply_schema_creates_the_document_pair() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("g.duckdb")).unwrap();
        store.apply_schema(&cvi_dataset()).unwrap();
        for t in ["cvi_params_document_live", "cvi_params_document_archive"] {
            let n: i64 = store.writer().query_row(&format!("select count(*) from {t}"), [], |r| r.get(0)).unwrap();
            assert_eq!(n, 0);
        }
    }
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p geode-data store::ddl` → compile errors.

- [ ] **Step 3: Implement the DDL half**

```rust
/// A dataset's live/archive pair. The measure family has one per grain,
/// the document family exactly one (market-data spec §4.1); everything
/// that publishes, sweeps or resolves history takes a pair rather than a
/// `Grain` so the two families go through one door.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TablePair {
    pub live: String,
    pub archive: String,
}

impl TablePair {
    pub fn for_grain(dataset: &str, grain: Grain) -> TablePair {
        TablePair {
            live: table_name(dataset, grain, TableKind::Live),
            archive: table_name(dataset, grain, TableKind::Archive),
        }
    }
    /// `document` is not a `Grain::short()` value, so this can never
    /// collide with a grain table of the same dataset.
    pub fn for_document(dataset: &str) -> TablePair {
        TablePair {
            live: format!("{dataset}_document{}", TableKind::Live.suffix()),
            archive: format!("{dataset}_document{}", TableKind::Archive.suffix()),
        }
    }
    pub fn of(&self, kind: TableKind) -> &str {
        match kind {
            TableKind::Live => &self.live,
            TableKind::Archive => &self.archive,
        }
    }
}

pub fn table_pairs(ds: &DatasetSpec) -> Vec<TablePair> {
    if ds.is_document() {
        vec![TablePair::for_document(&ds.name)]
    } else {
        ds.grains().into_iter().map(|g| TablePair::for_grain(&ds.name, g)).collect()
    }
}

/// The document family's table: `DatasetSpec::document_columns` in that
/// order, then the same four storage columns every grain table carries
/// (see `create_table_sql` for why `gen_id`/`source_time` ride on live).
pub fn create_document_table_sql(ds: &DatasetSpec, kind: TableKind) -> String {
    let mut cols: Vec<String> = ds
        .document_columns()
        .iter()
        .map(|c| format!("  \"{}\" {}", c.name, c.ty.sql()))
        .collect();
    cols.push("  \"batch\" VARCHAR".to_string());
    cols.push("  \"source_file_id\" BIGINT".to_string());
    cols.push("  \"gen_id\" BIGINT".to_string());
    cols.push("  \"source_time\" TIMESTAMP WITH TIME ZONE".to_string());
    format!(
        "CREATE TABLE IF NOT EXISTS {} (\n{}\n);",
        TablePair::for_document(&ds.name).of(kind),
        cols.join(",\n")
    )
}

pub fn history_of(dataset: &str, ds: &DatasetSpec) -> Vec<String> {
    let _ = dataset; // the dataset's own name is `ds.name`; kept for the callers' shape
    table_pairs(ds)
        .into_iter()
        .flat_map(|p| [p.archive, p.live])
        .collect()
}
```

(Check the existing `history_of` body's archive/live order and keep it — the assertion above assumes archive first; adjust the test to whatever order it already produces.)

`Store::apply_schema`:

```rust
    /// Create the live and archive pair(s) a dataset owns: one per grain
    /// for the measure family, one for the document family. Idempotent.
    pub fn apply_schema(&self, ds: &DatasetSpec) -> Result<(), StoreError> {
        for kind in [TableKind::Live, TableKind::Archive] {
            let statements: Vec<String> = if ds.is_document() {
                vec![ddl::create_document_table_sql(ds, kind)]
            } else {
                ds.grains().into_iter().map(|g| ddl::create_table_sql(ds, g, kind)).collect()
            };
            for sql in statements {
                self.writer.execute_batch(&sql).map_err(|source| StoreError::Sql { statement: sql, source })?;
            }
        }
        Ok(())
    }
```

- [ ] **Step 4: Run the DDL tests** — PASS. Run `zsh scripts/mutation-check.sh --anchors-only`; re-anchor anything in `ddl.rs` it reports.

- [ ] **Step 5: Refactor `publish_file` to take `TablePair`**

Replace `pub grain: Grain` in `PublishRequest` with `pub tables: TablePair`, and in `publish_file` replace

```rust
    let live = table_name(&req.dataset, req.grain, TableKind::Live);
    let archive = table_name(&req.dataset, req.grain, TableKind::Archive);
```

with

```rust
    let live = &req.tables.live;
    let archive = &req.tables.archive;
```

Rewrite the struct's doc comment: "`dataset` names the summary row; `tables` names the pair the rows move between — a grain's pair for the measure family, the one document pair for the document family. Two datasets can share a grain, so a pair is never derived from a grain alone." Fix every constructor: `load.rs:~227` becomes `tables: TablePair::for_grain(req.dataset_name, *grain)`; every `PublishRequest { grain: … }` in `publish.rs`, `retention.rs`, `catalog.rs` (store) and `service.rs` tests becomes `tables: TablePair::for_grain(<dataset>, <grain>)`. Let the compiler find them.

- [ ] **Step 6: Refactor `retention::sweep` to take `&[TablePair]`**

Change the signature and `sweep_in_transaction` to iterate pairs; wherever the body built `table_name(&ds.name, grain, kind)`, use `pair.of(kind)`. Rewrite its doc comment ("eviction per pair, then `reconcile_generations` once every pair has been swept"). Update every caller (`service.rs`'s sweeper and every test) to pass `&ddl::table_pairs(ds)` where it passed `&ds.grains()`.

- [ ] **Step 7: Run everything** — `cargo test -p geode-data` → PASS with every existing test's meaning intact; `cargo clippy --workspace --all-targets -- -D warnings`; `zsh scripts/mutation-check.sh --anchors-only` → exit 0 (re-anchor any moved lines in `publish.rs`, `retention.rs`, `load.rs`, `service.rs`).

- [ ] **Step 8: Commit**

```bash
git add crates/geode-data scripts/mutation-check.sh
git commit -m "store: TablePair — publish, sweep and history take a live/archive pair, not a grain; document DDL"
```

---

### Task 7: `publish_document`

**Files:**
- Create: `crates/geode-data/src/store/document.rs`
- Modify: `crates/geode-data/src/store/mod.rs` (`pub mod document;`)
- Test: `crates/geode-data/src/store/document.rs` `mod tests`
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Consumes: Task 4's `DocumentRows`, `join_key`; Task 6's `TablePair`, `publish_file`, `PublishRequest`; `Catalog::{reserve_file_id, reserve_gen_id, live_source_time, record}`; `ddl::{refresh_enum, categorical_columns}`.
- Produces:

```rust
pub struct DocumentPublishRequest<'a> {
    pub dataset: &'a DatasetSpec,
    pub source: &'a str,           // for the file_generations path and the Published event
    pub rows: &'a DocumentRows,
    pub source_time: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
    pub bytes: u64,                // the message's size, for provenance
}
pub struct DocumentPublished { pub batch: String, pub gen_id: i64, pub rows: usize, pub outcome: PublishOutcome }
pub fn publish_document(store: &Store, req: &DocumentPublishRequest) -> Result<DocumentPublished, StoreError>
pub const STAGING_TABLE: &str = "staging_document";
```

`StoreError` needs a variant for a validation failure that is not SQL: add `StoreError::Document(String)` (Display: "document: {0}") — `DocumentRows::validate`'s message wrapped.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::as_of::resolve_generations;
    use crate::store::catalog::Catalog;
    use crate::store::ddl::{TablePair, assert_generations_match_tables, table_pairs};
    use crate::store::retention::{RetentionPolicy, sweep};
    use chrono::{DateTime, NaiveDate, Utc};
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::document::{Column, DocumentRows, Value};
    use geode_core::schema::{DatasetSpec, SchemaSpec};

    const CVI: &str = /* the Task 1 fixture, verbatim */;

    fn cvi() -> DatasetSpec { /* as in Task 4's tests */ }
    fn ts(s: &str) -> DateTime<Utc> { DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc) }
    fn d(s: &str) -> NaiveDate { NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap() }

    fn doc(key: &str, params: [f64; 6]) -> DocumentRows {
        DocumentRows {
            key: vec![key.into()],
            attributes: vec![("anchor_date".into(), Value::Date(d("2026-09-12"))), ("spot_ref".into(), Value::F64(7650.0))],
            axes: vec![
                ("term".into(), Column::Date(vec![d("2026-09-18"), d("2026-09-18"), d("2026-09-18"), d("2026-10-16"), d("2026-10-16"), d("2026-10-16")])),
                ("node".into(), Column::F64(vec![-20.0, -1.0, 3.5, -20.0, -1.0, 3.5])),
            ],
            values: vec![("param".into(), Column::F64(params.to_vec()))],
        }
    }

    fn fixture() -> (tempfile::TempDir, Store, DatasetSpec) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        let ds = cvi();
        store.apply_schema(&ds).unwrap();
        Catalog::new(store.writer()).ensure_tables().unwrap();
        (dir, store, ds)
    }

    fn publish(store: &Store, ds: &DatasetSpec, rows: &DocumentRows, at: &str) -> DocumentPublished {
        publish_document(store, &DocumentPublishRequest {
            dataset: ds, source: "cvi", rows, source_time: ts(at), received_at: ts(at), bytes: 1234,
        }).unwrap()
    }

    fn live_params(store: &Store, key: &str) -> Vec<f64> {
        let mut stmt = store.writer().prepare(
            "select param from cvi_params_document_live where underlying_ref = ? order by term, node").unwrap();
        stmt.query_map(duckdb::params![key], |r| r.get::<_, f64>(0)).unwrap().collect::<Result<_, _>>().unwrap()
    }

    #[test]
    fn a_first_publish_lands_live_with_the_key_as_batch_and_no_book() {
        let (_d, store, ds) = fixture();
        let out = publish(&store, &ds, &doc("SPX.Z", [1., 2., 3., 4., 5., 6.]), "2026-09-12T14:00:00Z");
        assert_eq!(out.batch, "SPX.Z");
        assert_eq!(out.rows, 6);
        assert!(matches!(out.outcome, PublishOutcome::Published { rows: 6 }));
        assert_eq!(live_params(&store, "SPX.Z"), vec![1., 2., 3., 4., 5., 6.]);
        let (batch, book, gen): (String, Option<String>, i64) = store.writer().query_row(
            "select batch, book, gen_id from generations where dataset = 'cvi_params'", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!((batch.as_str(), book, gen), ("SPX.Z", None, out.gen_id));
        // The attributes ride on every row.
        let spot: f64 = store.writer().query_row("select min(spot_ref) from cvi_params_document_live", [], |r| r.get(0)).unwrap();
        assert_eq!(spot, 7650.0);
        assert_generations_match_tables(store.writer(), "cvi_params", &crate::store::ddl::history_of("cvi_params", &ds));
    }

    #[test]
    fn a_republish_replaces_live_and_archives_the_previous_generation() {
        let (_d, store, ds) = fixture();
        let first = publish(&store, &ds, &doc("SPX.Z", [1., 1., 1., 1., 1., 1.]), "2026-09-12T14:00:00Z");
        let second = publish(&store, &ds, &doc("SPX.Z", [2., 2., 2., 2., 2., 2.]), "2026-09-12T14:05:00Z");
        assert_eq!(live_params(&store, "SPX.Z"), vec![2.; 6]);
        let archived: i64 = store.writer().query_row(
            "select count(*) from cvi_params_document_archive where gen_id = ?", duckdb::params![first.gen_id], |r| r.get(0)).unwrap();
        assert_eq!(archived, 6);
        assert_ne!(first.gen_id, second.gen_id);
    }

    #[test]
    fn two_keys_are_two_partitions_that_do_not_disturb_each_other() {
        let (_d, store, ds) = fixture();
        publish(&store, &ds, &doc("SPX.Z", [1.; 6]), "2026-09-12T14:00:00Z");
        publish(&store, &ds, &doc("NDX.Z", [9.; 6]), "2026-09-12T14:01:00Z");
        publish(&store, &ds, &doc("SPX.Z", [2.; 6]), "2026-09-12T14:02:00Z");
        assert_eq!(live_params(&store, "SPX.Z"), vec![2.; 6]);
        assert_eq!(live_params(&store, "NDX.Z"), vec![9.; 6]);
    }

    #[test]
    fn an_older_document_is_archived_only_and_live_is_untouched() {
        let (_d, store, ds) = fixture();
        publish(&store, &ds, &doc("SPX.Z", [5.; 6]), "2026-09-12T14:05:00Z");
        let out = publish(&store, &ds, &doc("SPX.Z", [1.; 6]), "2026-09-12T14:00:00Z");
        assert!(matches!(out.outcome, PublishOutcome::ArchivedOnly { .. }));
        assert_eq!(live_params(&store, "SPX.Z"), vec![5.; 6]);
    }

    #[test]
    fn as_of_resolves_the_generation_live_at_that_instant() {
        let (_d, store, ds) = fixture();
        let first = publish(&store, &ds, &doc("SPX.Z", [1.; 6]), "2026-09-12T14:00:00Z");
        publish(&store, &ds, &doc("SPX.Z", [2.; 6]), "2026-09-12T14:05:00Z");
        let resolved = resolve_generations(store.writer(), "cvi_params", ts("2026-09-12T14:02:00Z")).unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].gen_id, first.gen_id);
        assert_eq!(resolved[0].book, None);
    }

    #[test]
    fn retention_by_count_sweeps_document_batches() {
        let (_d, store, ds) = fixture();
        for (i, at) in ["14:00", "14:01", "14:02"].iter().enumerate() {
            publish(&store, &ds, &doc("SPX.Z", [i as f64; 6]), &format!("2026-09-12T{at}:00Z"));
        }
        let policy = RetentionPolicy { keep_generations: Some(2), keep_age: None };
        sweep(store.writer(), &ds, &table_pairs(&ds), &policy, ts("2026-09-12T15:00:00Z")).unwrap();
        let gens: i64 = store.writer().query_row("select count(*) from generations where dataset = 'cvi_params'", [], |r| r.get(0)).unwrap();
        assert_eq!(gens, 2, "the oldest generation is evicted");
        assert_eq!(live_params(&store, "SPX.Z"), vec![2.; 6], "live is never swept");
        assert_generations_match_tables(store.writer(), "cvi_params", &crate::store::ddl::history_of("cvi_params", &ds));
    }

    #[test]
    fn the_file_generations_row_records_the_document_with_a_synthetic_path() {
        let (_d, store, ds) = fixture();
        let out = publish(&store, &ds, &doc("SPX.Z", [1.; 6]), "2026-09-12T14:00:00Z");
        let (path, size, rows, health): (String, i64, i64, String) = store.writer().query_row(
            "select path, size, row_count, health from file_generations where gen_id = ?", duckdb::params![out.gen_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap();
        assert_eq!(path, "document://cvi/cvi_params/SPX.Z");
        assert_eq!((size, rows, health.as_str()), (1234, 6, "ok"));
    }

    #[test]
    fn the_key_dimension_enum_is_refreshed_after_publish() {
        let (_d, store, ds) = fixture();
        publish(&store, &ds, &doc("SPX.Z", [1.; 6]), "2026-09-12T14:00:00Z");
        publish(&store, &ds, &doc("NDX.Z", [1.; 6]), "2026-09-12T14:00:00Z");
        let n: i64 = store.writer().query_row(
            "select count(*) from (select unnest(enum_range(NULL::cvi_params_underlying_ref_enum)))", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn an_invalid_document_is_refused_before_anything_is_written() {
        let (_d, store, ds) = fixture();
        let mut bad = doc("SPX.Z", [1.; 6]);
        bad.values[0].1 = Column::F64(vec![1.; 5]);
        let err = publish_document(&store, &DocumentPublishRequest {
            dataset: &ds, source: "cvi", rows: &bad, source_time: ts("2026-09-12T14:00:00Z"), received_at: ts("2026-09-12T14:00:00Z"), bytes: 0,
        }).unwrap_err();
        assert!(err.to_string().contains("value 'param' has 5 rows"), "{err}");
        let n: i64 = store.writer().query_row("select count(*) from file_generations", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn a_two_column_key_joins_with_the_separator() {
        // Correlation-shaped: key = [underlying_ref, underlying2_ref].
        let text = CVI
            .replace("key = [\"underlying_ref\"]", "key = [\"underlying_ref\", \"underlying2_ref\"]")
            + "\n[cvi_params.columns.underlying2_ref]\ntype = \"utf8\"\nrole = \"dimension\"\n";
        let doc_ = merge_docs("datasets", &[LayerDoc::builtin("datasets", &text).unwrap()]);
        let (schema, diags) = SchemaSpec::from_doc(&doc_);
        assert!(diags.is_empty(), "{diags:?}");
        let ds = schema.dataset("cvi_params").unwrap().clone();
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        store.apply_schema(&ds).unwrap();
        Catalog::new(store.writer()).ensure_tables().unwrap();
        let mut rows = doc("SPX.Z", [1.; 6]);
        rows.key.push("NDX.Z".into());
        let out = publish(&store, &ds, &rows, "2026-09-12T14:00:00Z");
        assert_eq!(out.batch, geode_core::document::join_key(&["SPX.Z".to_string(), "NDX.Z".to_string()]));
        assert_eq!(geode_core::document::split_key(&out.batch), vec!["SPX.Z".to_string(), "NDX.Z".to_string()]);
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p geode-data store::document` → module missing.

- [ ] **Step 3: Implement**

```rust
//! Publishing a parsed document (market-data spec §4.1): stage the rows
//! through DuckDB's appender, then run the same transaction a CSV file's
//! grain tables go through (`publish_file`) against the dataset's one
//! document pair. The message is the file: its key is the batch, its
//! book is empty, and every downstream mechanism — generations, the
//! backfill guard, as-of, retention, the freshness catalog — is reused
//! rather than reimplemented.

use crate::store::catalog::{Catalog, FileGeneration};
use crate::store::ddl::{self, TablePair};
use crate::store::publish::{Partition, PublishOutcome, PublishRequest, publish_file};
use crate::store::{Store, StoreError};
use chrono::{DateTime, Utc};
use geode_core::document::{Column, DocumentRows, Value, join_key};
use geode_core::health::Health;
use geode_core::schema::DatasetSpec;
use std::path::PathBuf;

/// One global staging table, like `staging_raw`: the ingest runner is
/// the single writer, so two documents never stage concurrently (see
/// `docs/ingest-cold-start-handoff.md` for why that invariant matters).
pub const STAGING_TABLE: &str = "staging_document";

pub struct DocumentPublishRequest<'a> {
    pub dataset: &'a DatasetSpec,
    pub source: &'a str,
    pub rows: &'a DocumentRows,
    pub source_time: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
    pub bytes: u64,
}

#[derive(Debug)]
pub struct DocumentPublished {
    pub batch: String,
    pub gen_id: i64,
    pub rows: usize,
    pub outcome: PublishOutcome,
}

/// The synthetic `file_generations.path` for a document: there is no
/// file, but provenance still wants one string a person can read.
pub fn document_path(source: &str, dataset: &str, batch: &str) -> PathBuf {
    PathBuf::from(format!("document://{source}/{dataset}/{batch}"))
}

pub fn publish_document(store: &Store, req: &DocumentPublishRequest) -> Result<DocumentPublished, StoreError> {
    req.rows.validate(req.dataset).map_err(StoreError::Document)?;
    let conn = store.writer();
    let ds = req.dataset;
    let batch = join_key(&req.rows.key);
    let catalog = Catalog::new(conn);
    let file_id = catalog.reserve_file_id()?;
    let gen_id = catalog.reserve_gen_id()?;

    // 1. Stage. `create or replace` so a failed previous attempt leaves
    // nothing behind; the column list is `document_columns` order, the
    // same order the table was created in (ddl::create_document_table_sql).
    let columns = ds.document_columns();
    let create = format!(
        "create or replace table {STAGING_TABLE} ({}, \"batch\" VARCHAR, \"source_file_id\" BIGINT)",
        columns.iter().map(|c| format!("\"{}\" {}", c.name, c.ty.sql())).collect::<Vec<_>>().join(", ")
    );
    conn.execute_batch(&create).map_err(|source| StoreError::Sql { statement: create.clone(), source })?;

    let rows = req.rows.rows();
    {
        let mut app = conn.appender(STAGING_TABLE).map_err(|source| StoreError::Sql { statement: "appender".into(), source })?;
        // Column accessors by position in `document_columns` order: key
        // parts, then axes, then values, then attributes — no row object
        // is built; each cell is read from its column at index `i`.
        let axis_cols: Vec<&Column> = req.rows.axes.iter().map(|(_, c)| c).collect();
        let value_cols: Vec<&Column> = ds
            .columns
            .iter()
            .filter(|c| c.role == geode_core::schema::ColumnRole::Value)
            .map(|c| &req.rows.values.iter().find(|(n, _)| n == &c.name).expect("validated").1)
            .collect();
        let attrs: Vec<&Value> = ds
            .columns
            .iter()
            .filter(|c| matches!(c.role, geode_core::schema::ColumnRole::Attribute { grain: None }))
            .map(|c| &req.rows.attributes.iter().find(|(n, _)| n == &c.name).expect("validated").1)
            .collect();
        for i in 0..rows {
            let mut cells: Vec<duckdb::types::Value> = Vec::with_capacity(columns.len() + 2);
            for part in &req.rows.key {
                cells.push(duckdb::types::Value::Text(part.clone()));
            }
            for col in &axis_cols {
                cells.push(cell(col, i));
            }
            for col in &value_cols {
                cells.push(cell(col, i));
            }
            for v in &attrs {
                cells.push(value(v));
            }
            cells.push(duckdb::types::Value::Text(batch.clone()));
            cells.push(duckdb::types::Value::BigInt(file_id));
            app.append_row(duckdb::params_from_iter(cells.iter()))
                .map_err(|source| StoreError::Sql { statement: format!("append row {i} into {STAGING_TABLE}"), source })?;
        }
        app.flush().map_err(|source| StoreError::Sql { statement: "appender flush".into(), source })?;
    }

    // 2. Publish through the shared transaction. One partition: the key,
    // no book. The backfill guard reads the live source time the same
    // way `load_file` does.
    let live_source_time = catalog.live_source_time(&ds.name, &batch, None)?;
    let tables = TablePair::for_document(&ds.name);
    let outcome = publish_file(
        conn,
        &PublishRequest {
            dataset: ds.name.clone(),
            tables: tables.clone(),
            staging_table: STAGING_TABLE.to_string(),
            partitions: vec![Partition { batch: batch.clone(), book: None }],
            gen_id,
            source_time: req.source_time,
            live_source_time,
        },
    )?;

    // 3. Dictionary refresh, so the query path can cast the key
    // dimension to its ENUM (same reasoning as `load_file` step 5).
    for col in ddl::categorical_columns(ds) {
        ddl::refresh_enum(conn, &ds.name, col, &tables.live, &tables.archive)?;
    }

    // 4. Provenance. `size` is the message's byte length and `mtime` its
    // receive time — the two things a document has where a file has a
    // stat.
    catalog.record(&FileGeneration {
        file_id,
        dataset: ds.name.clone(),
        batch: batch.clone(),
        path: document_path(req.source, &ds.name, &batch),
        size: req.bytes,
        mtime: req.received_at,
        source_time: req.source_time,
        gen_id,
        loaded_at: Utc::now(),
        row_count: rows,
        books: vec![None],
        archived_only: matches!(outcome, PublishOutcome::ArchivedOnly { .. }),
        health: Health::Ok,
    })?;

    Ok(DocumentPublished { batch, gen_id, rows, outcome })
}

fn cell(col: &Column, i: usize) -> duckdb::types::Value {
    match col {
        Column::F64(v) => duckdb::types::Value::Double(v[i]),
        Column::I64(v) => duckdb::types::Value::BigInt(v[i]),
        Column::Utf8(v) => duckdb::types::Value::Text(v[i].clone()),
        Column::Date(v) => duckdb::types::Value::Date32(days_since_epoch(v[i])),
    }
}

fn value(v: &Value) -> duckdb::types::Value {
    match v {
        Value::F64(x) => duckdb::types::Value::Double(*x),
        Value::I64(x) => duckdb::types::Value::BigInt(*x),
        Value::Utf8(s) => duckdb::types::Value::Text(s.clone()),
        Value::Date(d) => duckdb::types::Value::Date32(days_since_epoch(*d)),
    }
}

fn days_since_epoch(d: chrono::NaiveDate) -> i32 {
    (d - chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()).num_days() as i32
}
```

If `duckdb::types::Value::Date32` is not the variant name at 1.10505, use whatever `duckdb::types::Value` offers for a `DATE` (check `duckdb::types::Value`'s docs in `~/.cargo/registry`), or append dates as `Text("YYYY-MM-DD")` into a `VARCHAR` staging column and cast in the publish `insert … select` — the latter needs `publish_file` to project rather than `select *`, so prefer the typed variant. `Health` is `geode_core::health::Health` (re-exported by `geode_data`).

Add `Document(String)` to `StoreError` with a `Display` arm `"document: {0}"`. Add `pub mod document;` to `store/mod.rs`.

- [ ] **Step 4: Run to verify pass** — `cargo test -p geode-data store::document` → PASS; then `cargo test -p geode-data` → PASS; clippy clean.

- [ ] **Step 5: Mutation entries**

```zsh
run_mutation "document: an invalid document is refused before staging" \
  crates/geode-data/src/store/document.rs \
  '    req.rows.validate(req.dataset).map_err(StoreError::Document)?;' \
  '    let _ = req.rows.validate(req.dataset);' \
  geode-data an_invalid_document_is_refused_before_anything_is_written

run_mutation "document: the backfill guard reads the live source time" \
  crates/geode-data/src/store/document.rs \
  '    let live_source_time = catalog.live_source_time(&ds.name, &batch, None)?;' \
  '    let live_source_time = None;' \
  geode-data an_older_document_is_archived_only_and_live_is_untouched

run_mutation "document: the key is the batch" \
  crates/geode-data/src/store/document.rs \
  '            partitions: vec![Partition { batch: batch.clone(), book: None }],' \
  '            partitions: vec![Partition { batch: "doc".into(), book: None }],' \
  geode-data two_keys_are_two_partitions_that_do_not_disturb_each_other

run_mutation "document: the key dimension enum is refreshed" \
  crates/geode-data/src/store/document.rs \
  '        ddl::refresh_enum(conn, &ds.name, col, &tables.live, &tables.archive)?;' \
  '        let _ = (col, &tables);' \
  geode-data the_key_dimension_enum_is_refreshed_after_publish

run_mutation "document: archived_only is recorded from the outcome" \
  crates/geode-data/src/store/document.rs \
  '        archived_only: matches!(outcome, PublishOutcome::ArchivedOnly { .. }),' \
  '        archived_only: false,' \
  geode-data an_older_document_is_archived_only_and_live_is_untouched
```

The last entry needs `an_older_document_is_archived_only_and_live_is_untouched` to also assert the `file_generations.archived_only` flag — add `let flag: bool = store.writer().query_row("select archived_only from file_generations where gen_id = ?", duckdb::params![out.gen_id], |r| r.get(0)).unwrap(); assert!(flag);` to that test before committing.

- [ ] **Step 6: Commit**

```bash
git add crates/geode-data/src/store scripts/mutation-check.sh
git commit -m "store: publish_document — a parsed document publishes as a generation of its dataset"
```

---

### Task 8: The document request

**Files:**
- Modify: `crates/geode-core/src/query.rs` (`DocumentParams`)
- Create: `crates/geode-data/src/query/document.rs` (`compile_document`)
- Modify: `crates/geode-data/src/query/mod.rs` (`pub mod document;`)
- Modify: `crates/geode-data/src/service.rs` (`DataService::document`)
- Modify: `crates/geode-data/src/handle.rs` (`Request::Document`, `DataHandle::document`, `serve` arm)
- Test: `crates/geode-data/src/query/document.rs` and `service.rs` test modules; `handle.rs` tests
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Produces:

```rust
// geode-core query.rs
#[derive(Debug, Clone, PartialEq)]
pub struct DocumentParams { pub key: QueryKey, pub tag: u64, pub submitted: Instant, pub dataset: String, pub document_key: Vec<String>, pub as_of: AsOf }
// geode-data query/document.rs
pub fn compile_document(conn: &Connection, schema: &SchemaSpec, params: &DocumentParams) -> Result<CompiledQuery, StoreError>
// geode-data service.rs
impl DataService { pub fn document(&self, params: &DocumentParams) -> Result<QueryId, StoreError> }
// geode-data handle.rs
pub enum Request { …, Document(DocumentParams), … }
impl DataHandle { pub fn document(&self, params: DocumentParams) -> bool }
```

The outcome is a `DataEvent::Query(QueryOutcome)` — the tile route is unchanged. Column meta: every column `scope_semantics: Direct`; key and axis and attribute columns `attribution_by_depth: vec![Attribution::Additive]` (they are labels; the blotter's marker only looks at measures), value columns `vec![Attribution::DeterminedNonAdditive]` — a document value is real on its row and must never be totalled, which is exactly that variant's meaning. Grouping is empty, so the snapshot has depth 0 only and no tree.

- [ ] **Step 1: Write the failing compile tests**

In `query/document.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use crate::store::catalog::Catalog;
    use crate::store::document::{DocumentPublishRequest, publish_document};
    use geode_core::query::{AsOf, DocumentParams, QueryKey};
    // cvi(), doc(), ts(), d() copied from store::document's tests (a `#[cfg(test)]` item
    // in another module is not importable; keep the copies short and identical).

    fn fixture_with_two_generations() -> (tempfile::TempDir, Store, SchemaSpec, i64) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        let ds = cvi();
        store.apply_schema(&ds).unwrap();
        Catalog::new(store.writer()).ensure_tables().unwrap();
        let first = publish_document(&store, &DocumentPublishRequest {
            dataset: &ds, source: "cvi", rows: &doc("SPX.Z", [1., 2., 3., 4., 5., 6.]),
            source_time: ts("2026-09-12T14:00:00Z"), received_at: ts("2026-09-12T14:00:00Z"), bytes: 0 }).unwrap();
        publish_document(&store, &DocumentPublishRequest {
            dataset: &ds, source: "cvi", rows: &doc("SPX.Z", [10., 20., 30., 40., 50., 60.]),
            source_time: ts("2026-09-12T14:05:00Z"), received_at: ts("2026-09-12T14:05:00Z"), bytes: 0 }).unwrap();
        let mut schema = SchemaSpec::default();
        schema.datasets.push(ds);
        (dir, store, schema, first.gen_id)
    }

    fn params(dataset: &str, key: &[&str], as_of: AsOf) -> DocumentParams {
        DocumentParams { key: QueryKey(7), tag: 1, submitted: std::time::Instant::now(), dataset: dataset.into(),
                         document_key: key.iter().map(|s| s.to_string()).collect(), as_of }
    }

    fn run(store: &Store, compiled: &CompiledQuery) -> Vec<(String, f64)> {
        // term is the first axis; read (node, param) in result order.
        let mut stmt = store.writer().prepare(&compiled.sql).unwrap();
        stmt.query_map(duckdb::params_from_iter(compiled.params.iter()), |r| Ok((r.get::<_, String>("term")?, r.get::<_, f64>("param")?)))
            .unwrap().collect::<Result<_, _>>().unwrap()
    }

    #[test]
    fn a_live_document_query_selects_one_key_in_axis_order() {
        let (_d, store, schema, _) = fixture_with_two_generations();
        let compiled = compile_document(store.writer(), &schema, &params("cvi_params", &["SPX.Z"], AsOf::Live)).unwrap();
        let names: Vec<&str> = compiled.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["underlying_ref", "term", "node", "param", "anchor_date", "spot_ref"]);
        assert!(compiled.sql.contains("cvi_params_document_live"), "{}", compiled.sql);
        assert!(compiled.sql.to_lowercase().contains("order by \"term\", \"node\""), "{}", compiled.sql);
        let rows = run(&store, &compiled);
        assert_eq!(rows.iter().map(|(_, p)| *p).collect::<Vec<_>>(), vec![10., 20., 30., 40., 50., 60.]);
        assert_eq!(compiled.stalest_input, vec!["cvi_params".to_string()]);
        assert!(compiled.grouping.is_empty());
        let param = compiled.columns.iter().find(|c| c.name == "param").unwrap();
        assert_eq!(param.attribution_by_depth, vec![Attribution::DeterminedNonAdditive]);
        assert_eq!(param.scope_semantics, ScopeSemantics::Direct);
    }

    #[test]
    fn an_as_of_document_query_reads_the_resolved_generation_from_the_archive() {
        let (_d, store, schema, first_gen) = fixture_with_two_generations();
        let compiled = compile_document(store.writer(), &schema, &params("cvi_params", &["SPX.Z"], AsOf::At(ts("2026-09-12T14:02:00Z")))).unwrap();
        assert!(compiled.sql.contains("cvi_params_document_archive"), "{}", compiled.sql);
        assert!(compiled.sql.contains(&format!("gen_id = {first_gen}")), "{}", compiled.sql);
        let rows = run(&store, &compiled);
        assert_eq!(rows.iter().map(|(_, p)| *p).collect::<Vec<_>>(), vec![1., 2., 3., 4., 5., 6.]);
        assert_eq!(compiled.resolved_as_of.get("cvi_params").copied(), Some(ts("2026-09-12T14:00:00Z")));
    }

    #[test]
    fn an_unknown_key_compiles_and_returns_no_rows() {
        let (_d, store, schema, _) = fixture_with_two_generations();
        let compiled = compile_document(store.writer(), &schema, &params("cvi_params", &["RUT.Z"], AsOf::Live)).unwrap();
        assert!(run(&store, &compiled).is_empty());
    }

    #[test]
    fn the_wrong_family_or_arity_or_dataset_is_a_compile_error() {
        let (_d, store, mut schema, _) = fixture_with_two_generations();
        assert!(compile_document(store.writer(), &schema, &params("nonesuch", &["SPX.Z"], AsOf::Live)).unwrap_err().to_string().contains("unknown dataset"));
        assert!(compile_document(store.writer(), &schema, &params("cvi_params", &["SPX.Z", "NDX.Z"], AsOf::Live)).unwrap_err().to_string().contains("key has 2 parts"));
        schema.datasets[0].family = geode_core::schema::Family::Measures;
        assert!(compile_document(store.writer(), &schema, &params("cvi_params", &["SPX.Z"], AsOf::Live)).unwrap_err().to_string().contains("not a document dataset"));
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p geode-data query::document` → module missing.

- [ ] **Step 3: Implement `compile_document`**

```rust
//! The document request (market-data spec §7): one document by key, live
//! or as-of, as a plain select in axis order. Deliberately not a view:
//! there is no grouping, no aggregation and no scope — the key IS the
//! predicate — so the tree compiler has nothing to add and everything to
//! get wrong.

use crate::query::as_of::{generation_predicate, resolve_generations};
use crate::query::compile::{CompiledColumn, CompiledQuery};
use crate::store::StoreError;
use crate::store::ddl::TablePair;
use duckdb::Connection;
use geode_core::attribution::{Attribution, ScopeSemantics};
use geode_core::query::{AsOf, DocumentParams};
use geode_core::schema::{ColumnRole, SchemaSpec};
use std::collections::BTreeMap;

fn invalid(msg: String) -> StoreError {
    StoreError::Document(msg)
}

pub fn compile_document(conn: &Connection, schema: &SchemaSpec, params: &DocumentParams) -> Result<CompiledQuery, StoreError> {
    let ds = schema
        .dataset(&params.dataset)
        .ok_or_else(|| invalid(format!("unknown dataset '{}'", params.dataset)))?;
    if !ds.is_document() {
        return Err(invalid(format!("dataset '{}' is not a document dataset", ds.name)));
    }
    if params.document_key.len() != ds.key.len() {
        return Err(invalid(format!("key has {} parts, dataset '{}' declares {}", params.document_key.len(), ds.name, ds.key.len())));
    }

    let columns = ds.document_columns();
    let projection = columns.iter().map(|c| format!("\"{}\"", c.name)).collect::<Vec<_>>().join(", ");
    let order = ds.axes.iter().map(|a| format!("\"{a}\"")).collect::<Vec<_>>().join(", ");
    let key_predicate = ds.key.iter().map(|k| format!("\"{k}\" = ?")).collect::<Vec<_>>().join(" and ");
    let sql_params: Vec<duckdb::types::Value> = params.document_key.iter().map(|v| duckdb::types::Value::Text(v.clone())).collect();

    let tables = TablePair::for_document(&ds.name);
    let mut resolved_as_of = BTreeMap::new();
    let (table, era) = match &params.as_of {
        AsOf::Live => (tables.live.clone(), String::new()),
        AsOf::At(t) => {
            // Same resolution as a view query (spec §6.5): the newest
            // generation per partition at or before `t`, from the summary
            // table, aimed at the archive; live rows are never mixed in.
            let gens = resolve_generations(conn, &ds.name, *t)?;
            if let Some(oldest) = gens.iter().map(|g| g.source_time).min() {
                resolved_as_of.insert(ds.name.clone(), oldest);
            }
            let predicate = if gens.is_empty() { "false".to_string() } else { generation_predicate(&gens) };
            // Under as-of the resolved generation may still be live (it
            // is the newest); `publish_file` only ever moves rows out of
            // live, so read both tables under the generation predicate.
            (
                format!("(select * from {} union all select * from {})", tables.live, tables.archive),
                format!(" and ({predicate})"),
            )
        }
    };

    let sql = format!("select {projection} from {table} where {key_predicate}{era} order by {order}");

    let compiled_columns = columns
        .iter()
        .map(|c| CompiledColumn {
            name: c.name.clone(),
            grain: None,
            attribution_by_depth: vec![match c.role {
                // A document value is real on its row and must never be
                // totalled — the exact meaning of DeterminedNonAdditive.
                ColumnRole::Value => Attribution::DeterminedNonAdditive,
                _ => Attribution::Additive,
            }],
            scope_semantics: ScopeSemantics::Direct,
        })
        .collect();

    Ok(CompiledQuery {
        sql,
        params: sql_params,
        grouping: Vec::new(),
        columns: compiled_columns,
        stalest_input: vec![ds.name.clone()],
        resolved_as_of,
    })
}
```

Check how `generation_predicate` spells its columns (it references `batch`, `book`, `gen_id` unqualified — fine inside the `union all` subquery since both tables carry them). If the `resolve_generations` result includes partitions of other keys, the key predicate still narrows to one; that is correct and cheap.

Read `Snapshot::from_batches`'s meta check: the projection's column names must equal the meta names in order — they do by construction (both come from `document_columns`).

- [ ] **Step 4: Run to verify pass** — `cargo test -p geode-data query::document` → PASS.

- [ ] **Step 5: Wire the service and handle, with tests**

In `service.rs`, beside `distinct`:

```rust
    /// The document request (market-data spec §7): compiled by
    /// `compile_document` and submitted like a view query, so it shares
    /// the pool's cancellation and per-key coalescing and comes back as
    /// an ordinary `DataEvent::Query`.
    pub fn document(&self, params: &DocumentParams) -> Result<QueryId, StoreError> {
        let compiled = compile_document(&self.conn, &self.config.schema, params)?;
        let catalog = Catalog::new(&self.conn);
        let freshness = match &params.as_of {
            AsOf::Live => Freshness {
                dataset: params.dataset.clone(),
                as_of: catalog.dataset_as_of(&params.dataset, &[])?.map(|t| t.to_rfc3339()),
                generation: catalog.latest_gen_id()?,
            },
            AsOf::At(_) => Freshness {
                dataset: params.dataset.clone(),
                as_of: compiled.resolved_as_of.get(&params.dataset).map(|t| t.to_rfc3339()),
                generation: 0,
            },
        };
        let provenance = Provenance {
            datasets: vec![freshness],
            as_of_request: match &params.as_of { AsOf::Live => None, AsOf::At(t) => Some(t.to_rfc3339()) },
        };
        Ok(self.pool.submit(QueryRequest {
            key: params.key,
            tag: params.tag,
            submitted: params.submitted,
            view: ViewId(format!("document:{}:{}", params.dataset, params.document_key.join("/"))),
            grouping: Vec::new(),
            compiled,
            provenance,
            kind: RequestKind::Query,
        }))
    }
```

In `handle.rs`: add `Document(DocumentParams)` to `Request`, `pub fn document(&self, params: DocumentParams) -> bool { self.send(Request::Document(params)) }`, and the `serve` arm mirroring `Query`'s (a compile error is that key's `QueryOutcome` with `snapshot: Err(e.to_string())`).

Service test (in `service.rs` `mod tests`; the existing `service()` fixture has only `risk_snapshot`, so build a second fixture with the CVI dataset published through `publish_document`, then `DataService::open_channel` with that schema and no views — `views: Vec::new()` is accepted):

```rust
    #[test]
    fn a_document_request_returns_the_document_as_a_query_outcome() {
        let (_dir, svc, rx) = document_service();   // opens a store, applies the CVI schema, publishes SPX.Z twice, opens the service
        let p = DocumentParams { key: QueryKey(3), tag: 9, submitted: Instant::now(), dataset: "cvi_params".into(), document_key: vec!["SPX.Z".into()], as_of: AsOf::Live };
        svc.document(&p).unwrap();
        let out = next(&rx);
        assert_eq!((out.key, out.tag), (QueryKey(3), 9));
        let snap = out.snapshot.unwrap();
        assert_eq!(snap.rows(), 6);
        assert_eq!(snap.f64_value("param", 0), Some(10.0));
        assert_eq!(snap.text_value("underlying_ref", 0), Some("SPX.Z"));
        assert_eq!(snap.provenance().datasets[0].dataset, "cvi_params");
        assert!(snap.provenance().datasets[0].as_of.is_some());
        let bad = DocumentParams { dataset: "nonesuch".into(), ..p.clone() };
        assert!(svc.document(&bad).is_err());
        svc.shutdown();
    }
```

Handle test (in `handle.rs` tests, beside the existing `Request::Query` round-trip):

```rust
    #[test]
    fn document_requests_are_forwarded_with_their_key() {
        let (handle, rx) = DataHandle::for_tests();
        assert!(handle.document(DocumentParams { key: QueryKey(5), tag: 1, submitted: Instant::now(), dataset: "cvi_params".into(), document_key: vec!["SPX.Z".into()], as_of: AsOf::Live }));
        match rx.recv().unwrap() {
            Request::Document(p) => assert_eq!((p.key, p.document_key.as_slice()), (QueryKey(5), &["SPX.Z".to_string()][..])),
            other => panic!("{other:?}"),
        }
    }
```

And a `serve`-level test that a compile failure comes back as this key's outcome: follow the pattern of the existing test for `Request::Query`'s error arm in `handle.rs` (if none exists, add one for both).

- [ ] **Step 6: Catalog keys**

No code: `build_catalog`'s `partitions_for` already lists one partition per batch with `book: None` for a document dataset. Add one test to `query/catalog.rs` asserting that after two `publish_document`s for two keys, `dataset_catalog` shows two partitions whose `batch`es split back (`geode_core::document::split_key`) to the keys, each with one generation marked live.

- [ ] **Step 7: Run everything** — `cargo test --workspace`, clippy, fmt, `cargo bench --workspace --no-run`, `cargo check -p geode-shell --features test-support --all-targets`.

- [ ] **Step 8: Mutation entries and commit**

```zsh
run_mutation "document query: as-of reads the resolved generation, not live" \
  crates/geode-data/src/query/document.rs \
  '                format!(" and ({predicate})"),' \
  '                String::new(),' \
  geode-data an_as_of_document_query_reads_the_resolved_generation_from_the_archive

run_mutation "document query: rows come back in axis order" \
  crates/geode-data/src/query/document.rs \
  '    let sql = format!("select {projection} from {table} where {key_predicate}{era} order by {order}");' \
  '    let sql = format!("select {projection} from {table} where {key_predicate}{era} order by \"node\", \"term\"");' \
  geode-data a_live_document_query_selects_one_key_in_axis_order

run_mutation "document query: a value is DeterminedNonAdditive" \
  crates/geode-data/src/query/document.rs \
  '                ColumnRole::Value => Attribution::DeterminedNonAdditive,' \
  '                ColumnRole::Value => Attribution::Additive,' \
  geode-data a_live_document_query_selects_one_key_in_axis_order

run_mutation "document query: the wrong family is refused" \
  crates/geode-data/src/query/document.rs \
  '    if !ds.is_document() {' \
  '    if false {' \
  geode-data the_wrong_family_or_arity_or_dataset_is_a_compile_error

run_mutation "service: a document compile error is that key's outcome" \
  crates/geode-data/src/handle.rs \
  '<the first line of the Request::Document error arm in serve — copy verbatim once written>' \
  '<the same arm with the sink call removed>' \
  geode-data <the serve-level test's name>
```

Note the axis-order mutation must change the ORDER BY to something the fixture distinguishes: the fixture's params are term-major, so ordering by node first reorders them — verify the test fails under the mutation before committing the entry.

```bash
git add crates/geode-core/src/query.rs crates/geode-data scripts/mutation-check.sh
git commit -m "data: the document request — one document by key, live or as-of, as an ordinary query outcome"
```

---

### Task 9: Documentation and the demo dataset declaration

**Files:**
- Modify: `CLAUDE.md` (one paragraph after the "Groupable columns" paragraph)
- Modify: `docs/superpowers/specs/2026-09-12-geode-market-data-documents-design.md` §3.3 (one sentence)
- Modify: `examples/demo-config/datasets.toml` (append the `cvi_params` declaration from spec §3.1, with its comment)
- Modify: `crates/geode-app/src/demo.rs` test `the_demo_layer_is_complete_and_points_sources_at_the_directory` if it counts datasets
- Modify: `scripts/mutation-check.sh` header list of "run it after touching…" (add "the document family")

- [ ] **Step 1: Spec amendment**

In §3.3, replace "The conflict detector (Phase 2 §3.5) applies to document-level attributes: a value that varies within one document is a per-column conflict count in diagnostics, the same signal a coarse measure gives." with: "A document-level attribute cannot vary within one document by construction: `DocumentRows` carries one `Value` per attribute and `publish_document` writes it onto every row (Part 1 ruling). The §3.5 conflict detector therefore has nothing to detect here and is not run on document tables."

- [ ] **Step 2: Demo dataset**

Append to `examples/demo-config/datasets.toml` the `cvi_params` block from spec §3.1 with a comment: "Declared now so Part 2's demo bus has a dataset to feed; until then the table is created empty at startup and shows in the diagnostics tile's data section with no generations." Run `cargo test -p geode-app` and fix any dataset-count assertion. **Delete `$TMPDIR/geode-demo/<rows>-42` before running `--demo`**: `apply_schema` is `CREATE TABLE IF NOT EXISTS`, but a new dataset is a new table, so no deletion is strictly required here — say so in the commit message, and confirm by booting `cargo run -p geode-app -- --demo 1000` and opening the diagnostics data section (the `cvi_params` dataset appears with zero partitions).

- [ ] **Step 3: CLAUDE.md paragraph**

Add after the "Groupable columns" paragraph:

> **Market-data documents Part 1 (2026-09-12)** — `datasets.toml` has a second family: `family = "document"` with `key = [...]`, `axes = [...]`, `role = "axis"` / `"value"` columns and grainless `role = "attribute"` columns that are document-level (`ColumnRole::Attribute { grain: None }`). The two families are refused on each other's vocabulary per column (`validate_dataset` / `validate_document`), a document dataset has no grain, `DatasetSpec::document_columns()` is the one column order storage and the document request share, and `groupable_columns` on a document dataset is its dimensions alone (never an axis). `store::ddl::TablePair` replaced the `Grain` parameter of `publish_file`, `retention::sweep` and `apply_schema`: a measure dataset has one pair per grain, a document dataset exactly one (`{dataset}_document_live/_archive`). `store::document::publish_document` stages `geode_core::document::DocumentRows` (struct-of-arrays, in `geode-core` because `geode-data` dev-depends on the demo generator) through DuckDB's appender and runs the same publish transaction with the joined key (`geode_core::document::KEY_SEPARATOR`, `\u{1f}`) as the batch and no book, so generations, the backfill guard, as-of, retention and the catalog carry over untouched. `Request::Document(DocumentParams)` compiles a plain select in axis order (`query::document::compile_document`) and is delivered as an ordinary `DataEvent::Query`; a document value's attribution is `DeterminedNonAdditive`. `ScopeSemantics::NotApplicable` and `Scope::applicable_to` exist for Part 2's consumers and are unused by any query yet. Parts 2–4 (adapter tier, the panel, egress) are governed by `docs/superpowers/specs/2026-09-12-geode-market-data-documents-design.md`.

- [ ] **Step 4: Run the full harness on the changed files, detached**

```bash
git add -A && git commit -m "docs: Part 1 of market-data documents — CLAUDE.md, spec §3.3, demo dataset"
nohup zsh scripts/mutation-check.sh --changed=main > /tmp/mut.log 2>&1 &
```

When it finishes: every entry `caught`, no `SURVIVED`, `git status --porcelain` clean. Fix any survivor by strengthening its named test, not by deleting the entry.

- [ ] **Step 5: Final verification**

All five CI commands green on this checkout; `zsh scripts/mutation-check.sh --anchors-only` exits 0. Then hand off per `superpowers:finishing-a-development-branch`.

---

## Self-review

**Spec coverage (§3, §4, §7, §12 part 1):**
- §3.1 declaration → Task 1; multi-column key and separator → Tasks 4, 7 (`a_two_column_key_joins_with_the_separator`).
- §3.2 every rule → Task 2 (one test each; the `textual`/`categorical` carry-over is the last block of `validate_document`).
- §3.3 groupable/picker/grains → Task 3; conflict detector → amended in Task 9 (vacuous by construction).
- §3.4 `NotApplicable` and applicability → Task 5 (declared; no query consumes it in Part 1, as the spec says).
- §4.1 table, message-is-file, `publish_document`, reuse of the transaction → Tasks 6, 7.
- §4.2 source time → `DocumentPublishRequest.source_time` is the caller's; the `receive`/`document:<field>` choice is Part 2's source config. Backfill guard → Task 7 test.
- §4.3 health lanes → Part 2 (the tracker is untouched here); `file_generations.health` recorded `Ok` → Task 7.
- §4.4 retention → Task 7 test over `table_pairs`.
- §7 request, as-of, empty snapshot, catalog keys → Task 8 (steps 3, 5, 6).
- §12 part 1 sequencing → Tasks 1–9 in this order.

**Placeholders:** Task 5's second mutation entry and Task 8's last entry leave the anchor to be copied from code once written, with the instruction to do so — deliberate, since the exact line does not exist yet; everything else is concrete.

**Type consistency:** `DatasetSpec::{family, key, axes, is_document, document_columns}` (Tasks 1, 3) are what Tasks 4, 6, 7, 8 call; `TablePair::{for_grain, for_document, of}` and `table_pairs` (Task 6) are what Tasks 7, 8 call; `DocumentRows::{validate, rows}` and `join_key`/`split_key` (Task 4) are what Tasks 7, 8 call; `PublishRequest.tables` (Task 6) is what Task 7 fills; `StoreError::Document` (Task 7) is what Task 8's `invalid` builds; `DocumentParams` fields (Task 8) match between core, service, handle and the tests.
