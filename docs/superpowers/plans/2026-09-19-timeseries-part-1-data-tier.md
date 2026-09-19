# Timeseries Viewer Part 1 (Data Tier) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the data-tier half of the timeseries viewer: the `series` dataset family (bitemporal, append-only), the on-demand `Fetch` adapter shape, `Request::Fetch` with coverage subtraction, per-pair retention, series catalog rows and source catalogues, and a seeded `demo_series` adapter under `--demo`. No UI, no query compiler (Part 2), no chart (Part 3).

**Architecture:** A third `Family` beside `Measures` and `Document`, with a fixed implied column set stored in one table per dataset keyed `(source, series_id, ts, received_at)` plus a coverage table. Rows enter through one door, `store::series::append_series`, run on the ingest thread as a third work lane. A `FetchWorker` thread per fetch source runs the adapter's blocking `fetch`; the service thread subtracts loaded coverage before it queues a job. The outcome reaches the app as `DataEvent::SeriesFetched`, keyed by the `(identity, source)` pair.

**Tech Stack:** Rust 2024, DuckDB 1.10505.0 (`bundled`, `chrono`), `chrono`, `rand 0.9` (demo generator), criterion, `tempfile` in tests.

**Spec:** `docs/superpowers/specs/2026-09-19-geode-timeseries-viewer-design.md` §4, §5, §10, §11, §12 part 1. Read §2 (rulings) and §4.1 before starting.

## Global Constraints

- A series is identified by the pair `(identity, source)`; every log line, event and health key spells it `"{identity}@{source}"`.
- The five implied columns, in this order and never declared in TOML: `source VARCHAR`, `series_id VARCHAR`, `ts TIMESTAMP`, `received_at TIMESTAMP`, `value DOUBLE`. Both timestamps are UTC, stored as naive `TIMESTAMP`, bound and read as `BIGINT` microseconds through `make_timestamp(?)` / `epoch_us(ts)`. Never `TIMESTAMP WITH TIME ZONE` on these tables.
- Table names: `{dataset}_series` and `{dataset}_series_coverage`. The staging table is the fixed global `staging_series` (one writer, the ingest thread; see `docs/ingest-cold-start-handoff.md`).
- Spans are half-open `[from, to)` everywhere: `FetchRequest`, coverage rows, `missing_spans`.
- `append_series` is the ONE door rows enter by, whatever produced them. It runs on the ingest thread under `geode_core::panic::contained`, like every other publish.
- Nothing blocks a producer and every sink refusal is counted, never acted on (`EventSink`, `IngestSink`, `MessageSink` rule).
- The serve loop (`handle.rs::serve`) stays non-blocking: a fetch is queued to a worker thread, never run inline.
- No `Vec` allocation per row on the append path beyond the staged `Value` buffer the document publish already pays; `SeriesRows` is struct-of-arrays.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo check -p geode-shell --features test-support --all-targets` and `cargo bench --workspace --no-run` pass at the end of every task. Commit at the end of every task.
- `zsh scripts/mutation-check.sh --anchors-only` before the final merge (Task 10).
- Do not touch the query compiler, `Delivery`, the shell, or any module crate: Part 1 ends at `DataEvent::SeriesFetched` and one log-only arm in `bridge.rs`.

---

## File map

| File | Responsibility |
|---|---|
| `crates/geode-core/src/schema/mod.rs` | `Family::Series`, `SeriesRetention`, `DatasetSpec::{is_series, series_retention}`, `series_columns()`, `validate_series`, the `from_doc` reads for `retention`/`history`, the `groupable_columns` arm, the drop guard |
| `crates/geode-core/src/source_config.rs` | `SourceShape`, `SourceSpec::shape`, the fetch branch of `from_doc` |
| `crates/geode-core/src/query.rs` | `SeriesCatalog`, `DatasetCatalog::series`, `CatalogSnapshot::identities` |
| `crates/geode-data/src/adapter/mod.rs` | `FetchRequest`, `SeriesRows`, `Fetch`, `Adapter::fetch` |
| `crates/geode-data/src/store/series.rs` (new) | DDL, `append_series`, `coverage`, `missing_spans`, `sweep_pair`, `series_catalog` |
| `crates/geode-data/src/store/ddl.rs`, `store/mod.rs` | `table_pairs` series arm, `apply_schema` series arm |
| `crates/geode-data/src/query/catalog.rs` | series rows and identities in the catalog |
| `crates/geode-data/src/ingest/runner.rs` | `SeriesJob`, the third queue lane, `IngestEvent::{SeriesAppended, SeriesFailed}`, `append_one_series` |
| `crates/geode-data/src/ingest/fetch.rs` (new) | `FetchWorker`, `FetchWork`, `FetchOutcome` |
| `crates/geode-data/src/service.rs`, `handle.rs` | `DataEvent::SeriesFetched`, `Request::{Fetch, Identities}`, `FetchParams`, `DataService::{fetch, identities}`, the open-time resolution of fetch sources |
| `crates/geode-app/src/bridge.rs` | the log-only `SeriesFetched` arm |
| `crates/geode-app/src/demo_series.rs` (new), `main.rs`, `demo.rs`, `examples/demo-config/datasets.toml` | the demo adapter, two demo sources, the `[series]` dataset |
| `crates/geode-data/benches/append_series.rs` (new), `docs/perf.md` | the append bench |
| `scripts/mutation-check.sh`, `CLAUDE.md`, `docs/phase-history.md`, the spec | Task 10 |

---

### Task 1: `Family::Series` in the schema

**Files:**
- Modify: `crates/geode-core/src/schema/mod.rs` (`Family` at line 21, `DatasetSpec` at 37, `from_doc` at 199, `validate_dataset` at 330, `parse_column`'s `match family` at 862, `groupable_columns` at 131, the drop guard at 301)
- Modify: `crates/geode-core/src/scope/mod.rs:578`, `crates/geode-demo-data/src/documents.rs:344` (two `DatasetSpec` literals gain the new field)
- Test: `crates/geode-core/src/schema/mod.rs` (`mod tests`)

**Interfaces:**
- Produces: `Family::Series`; `pub struct SeriesRetention { pub retention: Option<Duration>, pub history: Option<Duration> }`; `DatasetSpec::series_retention: Option<SeriesRetention>` (`Some` only on the series family); `DatasetSpec::is_series(&self) -> bool`; `pub const SERIES_COLUMNS: [&str; 5] = ["source", "series_id", "ts", "received_at", "value"]`; `DatasetSpec::series_columns(&self) -> &'static [&'static str]`.

- [ ] **Step 1: Write the failing tests** (append inside `mod tests` in `schema/mod.rs`; the module must import `Severity`, `LayerDoc` and `merge_docs` from `crate::config` if it does not already)

```rust
    // --- the series family (timeseries spec §4.2, §4.3) --------------------

    fn series_schema(text: &str) -> (SchemaSpec, Vec<Diagnostic>) {
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        SchemaSpec::from_doc(&doc)
    }

    #[test]
    fn a_series_dataset_declares_no_columns_and_implies_five() {
        let (schema, diags) = series_schema(
            r#"
[series]
family = "series"
retention = "30d"
history = "5y"
"#,
        );
        assert!(diags.is_empty(), "{diags:?}");
        let ds = schema.dataset("series").unwrap();
        assert!(ds.is_series());
        assert!(!ds.is_document());
        assert!(ds.columns.is_empty());
        assert_eq!(
            ds.series_columns(),
            &["source", "series_id", "ts", "received_at", "value"]
        );
        let r = ds.series_retention.as_ref().unwrap();
        assert_eq!(r.retention, Some(std::time::Duration::from_secs(30 * 86_400)));
        assert_eq!(r.history, Some(std::time::Duration::from_secs(5 * 365 * 86_400)));
        assert!(ds.groupable_columns().is_empty(), "no scope reaches a series");
    }

    #[test]
    fn retention_and_history_default_to_unbounded() {
        let (schema, diags) = series_schema("[series]\nfamily = \"series\"\n");
        assert!(diags.is_empty(), "{diags:?}");
        let r = schema.dataset("series").unwrap().series_retention.as_ref().unwrap();
        assert_eq!((r.retention, r.history), (None, None));
    }

    #[test]
    fn a_declared_column_on_a_series_dataset_is_an_error_and_dropped() {
        let (schema, diags) = series_schema(
            r#"
[series]
family = "series"
[series.columns.value]
type = "f64"
role = "value"
"#,
        );
        let ds = schema.dataset("series").unwrap();
        assert!(ds.columns.is_empty(), "the column is dropped, the dataset kept");
        assert!(diags.iter().any(|d| {
            d.severity == Severity::Error
                && d.path.as_deref() == Some("datasets.series.columns.value")
                && d.message.contains("implies its columns")
        }), "{diags:?}");
    }

    #[test]
    fn key_axes_and_a_bad_duration_are_refused_on_a_series_dataset() {
        let (schema, diags) = series_schema(
            r#"
[series]
family = "series"
key = ["x"]
axes = ["y"]
retention = "soon"
"#,
        );
        let ds = schema.dataset("series").unwrap();
        assert!(ds.key.is_empty() && ds.axes.is_empty());
        assert_eq!(ds.series_retention.as_ref().unwrap().retention, None);
        for path in ["datasets.series.key", "datasets.series.axes", "datasets.series.retention"] {
            assert!(
                diags.iter().any(|d| d.severity == Severity::Error && d.path.as_deref() == Some(path)),
                "missing error at {path}: {diags:?}"
            );
        }
    }

    #[test]
    fn retention_on_a_measure_dataset_is_a_warning() {
        let (_, diags) = series_schema(
            r#"
[risk]
retention = "30d"
[risk.columns.book]
type = "utf8"
role = "dimension"
"#,
        );
        assert!(diags.iter().any(|d| d.severity == Severity::Warning
            && d.path.as_deref() == Some("datasets.risk.retention")), "{diags:?}");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p geode-core a_series_dataset_declares_no_columns_and_implies_five`
Expected: compile error, `no variant named Series` / `no method named is_series`.

- [ ] **Step 3: Add the variant, the retention struct, the field and the methods**

In `schema/mod.rs`, replace the `Family` enum and `parse`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Family {
    #[default]
    Measures,
    Document,
    /// Timeseries spec §4: a fixed five-column, bitemporal, append-only
    /// table per dataset. Declares no columns; `SERIES_COLUMNS` implies
    /// them.
    Series,
}

impl Family {
    pub fn parse(s: &str) -> Option<Family> {
        match s {
            "measures" => Some(Family::Measures),
            "document" => Some(Family::Document),
            "series" => Some(Family::Series),
            _ => None,
        }
    }
}

/// The series family's two retention windows (timeseries spec §4.7).
/// `retention` bounds superseded rows by `received_at`; `history`
/// bounds every row by `ts`. `None` is unbounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SeriesRetention {
    pub retention: Option<std::time::Duration>,
    pub history: Option<std::time::Duration>,
}

/// The series family's storage and projection order (timeseries spec
/// §4.3): the one list `store::series` DDL, `append_series` and Part 2's
/// query compiler share, so no two can disagree about a column's position.
pub const SERIES_COLUMNS: [&str; 5] = ["source", "series_id", "ts", "received_at", "value"];
```

Add the field to `DatasetSpec` after `axes`:

```rust
    /// Series family only: its retention windows. `None` on every other
    /// family, `Some` (possibly both unbounded) on a series dataset.
    pub series_retention: Option<SeriesRetention>,
```

Add the methods after `is_document`:

```rust
    pub fn is_series(&self) -> bool {
        self.family == Family::Series
    }

    /// The implied columns of a series dataset, in storage order. Answers
    /// the same five names for any dataset; only a series dataset's
    /// tables carry them.
    pub fn series_columns(&self) -> &'static [&'static str] {
        &SERIES_COLUMNS
    }
```

In `groupable_columns`, before the `if self.is_document()` branch:

```rust
        if self.is_series() {
            // Timeseries spec §4.3: no scope or grouping reaches a series;
            // the series query takes no scope at all.
            return Vec::new();
        }
```

In `from_doc`, after `let mut dataset = DatasetSpec { … };` (the literal at line 268 gains `series_retention: None,`), read the two windows:

```rust
            if family == Family::Series {
                let mut window = |field: &str| -> Option<std::time::Duration> {
                    let raw = ds_value.get(field).and_then(|v| v.as_str())?;
                    match crate::source_config::parse_duration(raw) {
                        Some(d) => Some(d),
                        None => {
                            diags.push(Diagnostic {
                                severity: Severity::Error,
                                layer: None,
                                file: None,
                                message: format!(
                                    "dataset '{ds_name}': '{field}' must be a duration such as \
                                     \"30d\", \"12h\" or \"5y\"; unbounded"
                                ),
                                path: Some(format!("datasets.{ds_name}.{field}")),
                            });
                            None
                        }
                    }
                };
                dataset.series_retention = Some(SeriesRetention {
                    retention: window("retention"),
                    history: window("history"),
                });
            } else {
                for field in ["retention", "history"] {
                    if ds_value.get(field).is_some() {
                        diags.push(note(
                            format!("datasets.{ds_name}.{field}"),
                            format!("dataset '{ds_name}': '{field}' applies to the series family only; ignored"),
                        ));
                    }
                }
            }
```

`parse_duration` today accepts `ms`, `s`, `m`, `h` only. Add `d` (86 400 s) and `y` (365 days) units to it in `source_config.rs` (`'d' => n.checked_mul(86_400)?`, `'y' => n.checked_mul(365 * 86_400)?`) and a line to its `durations_are_seconds_minutes_or_hours` test: `assert_eq!(parse_duration("30d"), Some(Duration::from_secs(30 * 86_400)));` and `assert_eq!(parse_duration("5y"), Some(Duration::from_secs(5 * 365 * 86_400)));`.

The measure-family `key`/`axes` warning loop (`if family == Family::Measures`) stays as it is; the series family's refusal of them is in `validate_series` below, an error, since the dataset shape is fixed.

Change the drop guard at line 301 to:

```rust
            if dataset.is_document() && dataset.columns.is_empty() {
```
unchanged, and add nothing for series: a series dataset with no columns is the correct state.

In `validate_dataset`, after the reserved-names loop and before `if ds.is_document()`:

```rust
    if ds.is_series() {
        diags.extend(validate_series(ds));
        return diags;
    }
```

Add `validate_series` after `validate_document`:

```rust
/// The series family's load-time rules (timeseries spec §4.3): the
/// family implies its columns, so any declared column is refused and
/// dropped, and `key`/`axes` are refused and cleared. The dataset itself
/// is always kept — there is nothing a trader can get wrong that makes
/// its five-column table unbuildable.
fn validate_series(ds: &mut DatasetSpec) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let name = ds.name.clone();
    let err = |message: String, path: String| Diagnostic {
        severity: Severity::Error,
        layer: None,
        file: None,
        message,
        path: Some(path),
    };
    for c in &ds.columns {
        diags.push(err(
            format!(
                "dataset '{name}' column '{}': the series family implies its columns \
                 (source, series_id, ts, received_at, value); column dropped",
                c.name
            ),
            format!("datasets.{name}.columns.{}", c.name),
        ));
    }
    ds.columns.clear();
    for (field, list) in [("key", &mut ds.key), ("axes", &mut ds.axes)] {
        if !list.is_empty() {
            diags.push(err(
                format!("dataset '{name}': '{field}' is not a series family key; ignored"),
                format!("datasets.{name}.{field}"),
            ));
            list.clear();
        }
    }
    diags
}
```

In `parse_column`'s `match family` (line 862):

```rust
            grain: match family {
                Family::Measures => Some(grain_of(table)?),
                Family::Document | Family::Series => None,
            },
```

Add `series_retention: None,` to the two other `DatasetSpec` literals: `crates/geode-core/src/scope/mod.rs:578` and `crates/geode-demo-data/src/documents.rs:344`.

- [ ] **Step 4: Run the tests and the workspace build**

Run: `cargo test -p geode-core schema:: && cargo check --workspace --all-targets`
Expected: the five new tests pass; every existing schema test still passes; the workspace compiles (the compiler names any other `DatasetSpec` literal; add the field there too).

- [ ] **Step 5: Commit**

```bash
git add crates/geode-core crates/geode-demo-data
git commit -m "core: the series dataset family — Family::Series, SeriesRetention, validate_series"
```

---

### Task 2: A fetch source in `SourceSpec`

**Files:**
- Modify: `crates/geode-core/src/source_config.rs` (`is_subscribed` at 142, `from_doc` at 339–720)
- Test: `crates/geode-core/src/source_config.rs` (`mod tests`, whose `schema()` fixture at ~line 690 gains a `[series]` stanza)

**Interfaces:**
- Produces: `pub enum SourceShape { Directory, Subscribed, Fetch }`; `SourceSpec::shape(&self, schema: &SchemaSpec) -> SourceShape`. `is_subscribed()` is unchanged (`adapter != csv_dir`, i.e. "not a directory") and every existing caller keeps its meaning; the service (Task 8) switches to `shape`.

- [ ] **Step 1: Write the failing tests** (append to `mod tests` in `source_config.rs`; extend `schema()`'s TOML with `[series]\nfamily = "series"\n` at the end)

```rust
    #[test]
    fn a_source_over_a_series_dataset_is_a_fetch_source() {
        let (specs, diags) = from(
            r#"
[kdb_hist]
adapter = "kdb"
dataset = "series"
"#,
        );
        assert!(diags.is_empty(), "{diags:?}");
        let s = &specs[0];
        assert_eq!(s.shape(&schema()), SourceShape::Fetch);
        assert!(s.is_subscribed(), "a fetch source is not a directory");
        assert_eq!(s.document, None);
        assert!(s.topics.is_empty());
        assert!(s.paths.is_empty());
    }

    #[test]
    fn subscribed_and_directory_keys_are_warned_on_a_fetch_source() {
        let (specs, diags) = from(
            r#"
[kdb_hist]
adapter = "kdb"
dataset = "series"
topics = ["a/>"]
document = "cvi_params"
paths = ["/x/*.csv"]
poll_interval = "2s"
"#,
        );
        assert_eq!(specs.len(), 1);
        assert!(specs[0].paths.is_empty(), "paths are dropped, not stored");
        for key in ["topics", "document", "paths", "poll_interval"] {
            assert!(
                diags.iter().any(|d| d.severity == Severity::Warning
                    && d.path.as_deref() == Some(&format!("sources.kdb_hist.{key}"))
                    && d.message.contains("fetch source")),
                "missing warning for {key}: {diags:?}"
            );
        }
    }

    #[test]
    fn a_csv_dir_source_over_a_series_dataset_is_refused() {
        let (specs, diags) = from(
            r#"
[files]
dataset = "series"
paths = ["/x/*.csv"]
"#,
        );
        assert!(specs.is_empty());
        assert!(diags.iter().any(|d| d.severity == Severity::Error
            && d.path.as_deref() == Some("sources.files.dataset")
            && d.message.contains("series")), "{diags:?}");
    }

    #[test]
    fn shape_names_all_three() {
        let (specs, _) = from(
            r#"
[a]
dataset = "risk_snapshot"
paths = ["/x/*.csv"]
[b]
adapter = "solace"
dataset = "cvi_params"
document = "cvi_params"
topics = ["t/>"]
[c]
adapter = "kdb"
dataset = "series"
"#,
        );
        let shapes: Vec<SourceShape> = specs.iter().map(|s| s.shape(&schema())).collect();
        assert_eq!(shapes, vec![SourceShape::Directory, SourceShape::Subscribed, SourceShape::Fetch]);
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-core source_config::tests::shape_names_all_three`
Expected: compile error, `SourceShape` not found.

- [ ] **Step 3: Implement**

Add after `SourceTime`:

```rust
/// Which of the three pipelines a source rides (timeseries spec §5.1):
/// the directory poller, a subscription receiver, or a fetch worker. The
/// dataset's family decides between the last two — a non-directory
/// adapter over a series dataset is fetched, over a document dataset
/// subscribed — so this takes the schema rather than storing a fourth
/// copy of the answer on the spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceShape {
    Directory,
    Subscribed,
    Fetch,
}
```

Add to `impl SourceSpec` after `is_subscribed`:

```rust
    pub fn shape(&self, schema: &SchemaSpec) -> SourceShape {
        if !self.is_subscribed() {
            SourceShape::Directory
        } else if schema.dataset(&self.dataset).is_some_and(|d| d.is_series()) {
            SourceShape::Fetch
        } else {
            SourceShape::Subscribed
        }
    }
```

In `from_doc`, replace the block at lines 374–395 (`let subscribed = …` through the "needs a document family dataset" `continue`) with:

```rust
            let subscribed = adapter != CSV_DIR_ADAPTER;
            let family = schema.dataset(&dataset).map(|d| d.family);
            let fetch = subscribed && family == Some(crate::schema::Family::Series);

            // A directory source reads CSV rows into grain tables; it can
            // never fill a series table (timeseries spec §5.1).
            if !subscribed && family == Some(crate::schema::Family::Series) {
                diags.push(diag(
                    Severity::Error,
                    name,
                    Some("dataset"),
                    format!(
                        "a directory source cannot fill the series dataset '{dataset}'; \
                         name a fetch adapter"
                    ),
                ));
                continue;
            }
            // A subscribed source has no CSV row to infer a shape from —
            // it publishes `DocumentRows` straight into a document-family
            // table, never a measures one (market-data-documents plan).
            if subscribed
                && !fetch
                && !schema.dataset(&dataset).is_some_and(|d| d.is_document())
            {
                diags.push(diag(
                    Severity::Error,
                    name,
                    Some("dataset"),
                    format!(
                        "adapter '{adapter}' needs a document family dataset; \
                         '{dataset}' is not one"
                    ),
                ));
                continue;
            }
```

Extend the two ignored-key loops (lines 397–431) so a fetch source warns on BOTH lists with its own wording. Replace them with:

```rust
            let ignored = |key: &str, diags: &mut Vec<Diagnostic>| {
                if !table.contains_key(key) {
                    return;
                }
                let m = if fetch {
                    format!("'{key}' is ignored by a fetch source (a series dataset)")
                } else if subscribed {
                    format!("'{key}' is ignored by a subscribed source (adapter != \"{CSV_DIR_ADAPTER}\")")
                } else {
                    format!("'{key}' is ignored by a directory source (adapter == \"{CSV_DIR_ADAPTER}\")")
                };
                diags.push(diag(Severity::Warning, name, Some(key), m));
            };
            for key in ["readiness", "poll_interval", "pending_timeout", "batch_pattern"] {
                if subscribed {
                    ignored(key, &mut diags);
                }
            }
            for key in ["document", "topics", "coalesce", "source_time"] {
                if !subscribed || fetch {
                    ignored(key, &mut diags);
                }
            }
```

In the `paths` block (line ~447), change the warning text to use the same `fetch` distinction: `if fetch { "'paths' is ignored by a fetch source (a series dataset)" } else { the existing text }`.

Change `let (document, topics, coalesce, source_time) = if subscribed {` to `if subscribed && !fetch {`, so a fetch source takes the `else` arm's defaults (`None`, empty, `DEFAULT_COALESCE`, `SourceTime::Receive`).

- [ ] **Step 4: Run the tests**

Run: `cargo test -p geode-core source_config`
Expected: all pass, including the existing subscribed-source tests (their wording is unchanged for the subscribed case).

- [ ] **Step 5: Commit**

```bash
git add crates/geode-core/src/source_config.rs
git commit -m "core: SourceShape — a non-directory source over a series dataset is a fetch source"
```

---

### Task 3: The `Fetch` adapter shape

**Files:**
- Modify: `crates/geode-data/src/adapter/mod.rs` (`Adapter` trait at 222–235; new items after `Egress`)
- Test: same file, `mod tests`

**Interfaces:**
- Produces:
```rust
pub struct FetchRequest { pub identity: String, pub from: DateTime<Utc>, pub to: DateTime<Utc> }
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SeriesRows { pub ts: Vec<DateTime<Utc>>, pub value: Vec<f64> }
impl SeriesRows {
    pub fn len(&self) -> usize; pub fn is_empty(&self) -> bool;
    /// Equal lengths and strictly ascending `ts`, or the reason.
    pub fn validate(&self) -> Result<(), AdapterError>;
    /// Drops NaN/±inf rows in place; returns how many.
    pub fn drop_non_finite(&mut self) -> usize;
}
pub trait Fetch: Send {
    fn fetch(&mut self, req: &FetchRequest) -> Result<SeriesRows, AdapterError>;
    fn catalogue(&mut self) -> Option<Vec<String>>;
}
// on Adapter:
fn fetch(&self) -> Option<Box<dyn Fetch>> { None }
```

- [ ] **Step 1: Write the failing tests** (in `adapter/mod.rs`'s `mod tests`, or create one if absent)

```rust
    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn series_rows_validate_lengths_and_order() {
        let ok = SeriesRows { ts: vec![t("2026-01-01T00:00:00Z"), t("2026-01-01T00:01:00Z")], value: vec![1.0, 2.0] };
        assert!(ok.validate().is_ok());
        let unequal = SeriesRows { ts: vec![t("2026-01-01T00:00:00Z")], value: vec![1.0, 2.0] };
        assert!(unequal.validate().unwrap_err().message.contains("2 values for 1 timestamp"));
        let unsorted = SeriesRows { ts: vec![t("2026-01-01T00:01:00Z"), t("2026-01-01T00:00:00Z")], value: vec![1.0, 2.0] };
        assert!(unsorted.validate().unwrap_err().message.contains("ascending"));
        let dup = SeriesRows { ts: vec![t("2026-01-01T00:00:00Z"), t("2026-01-01T00:00:00Z")], value: vec![1.0, 2.0] };
        assert!(dup.validate().unwrap_err().message.contains("ascending"), "a repeated ts is not strictly ascending");
    }

    #[test]
    fn drop_non_finite_keeps_the_arrays_aligned() {
        let mut rows = SeriesRows {
            ts: vec![t("2026-01-01T00:00:00Z"), t("2026-01-01T00:01:00Z"), t("2026-01-01T00:02:00Z")],
            value: vec![1.0, f64::NAN, f64::INFINITY],
        };
        assert_eq!(rows.drop_non_finite(), 2);
        assert_eq!(rows.ts, vec![t("2026-01-01T00:00:00Z")]);
        assert_eq!(rows.value, vec![1.0]);
    }

    #[test]
    fn an_adapter_has_no_fetch_side_by_default() {
        let (adapter, _feed) = ChannelAdapter::new("demo_bus");
        assert!(adapter.fetch().is_none());
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-data adapter::tests::series_rows_validate_lengths_and_order`
Expected: compile error, `SeriesRows` not found.

- [ ] **Step 3: Implement** (after the `Egress` trait, before `Adapter`)

```rust
/// One on-demand history request (timeseries spec §5.2): an identity the
/// source interprets, over a half-open span `from <= ts < to`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchRequest {
    pub identity: String,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}

/// What a fetch returns: struct-of-arrays, equal lengths, strictly
/// ascending `ts`. Never a row struct (PHILOSOPHY §6): the append path
/// reads both columns at index `i`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SeriesRows {
    pub ts: Vec<DateTime<Utc>>,
    pub value: Vec<f64>,
}

impl SeriesRows {
    pub fn len(&self) -> usize {
        self.ts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ts.is_empty()
    }

    /// Equal lengths and strictly ascending `ts`. A repeated timestamp is
    /// refused rather than resolved: which value wins is the source's
    /// question, not this crate's.
    pub fn validate(&self) -> Result<(), AdapterError> {
        if self.ts.len() != self.value.len() {
            return Err(AdapterError {
                message: format!(
                    "{} values for {} timestamps",
                    self.value.len(),
                    self.ts.len()
                ),
            });
        }
        if let Some(i) = (1..self.ts.len()).find(|&i| self.ts[i] <= self.ts[i - 1]) {
            return Err(AdapterError {
                message: format!(
                    "timestamps must be strictly ascending; row {i} ({}) follows {}",
                    self.ts[i], self.ts[i - 1]
                ),
            });
        }
        Ok(())
    }

    /// Removes rows whose value is NaN or infinite, keeping the two arrays
    /// aligned. Returns how many were dropped, for the warning line.
    pub fn drop_non_finite(&mut self) -> usize {
        let before = self.ts.len();
        let mut keep = self.value.iter().map(|v| v.is_finite());
        self.ts.retain(|_| keep.next().unwrap_or(false));
        self.value.retain(|v| v.is_finite());
        before - self.ts.len()
    }
}

/// The on-demand side (timeseries spec §5.2): history for one identity
/// over a span, at the source's native grain. `Send` and not `Sync` for
/// the reason [`Subscription`] is — one fetch worker thread owns it, and
/// `&mut self` is what lets a vendor client keep a connection inside it
/// with no lock. Called on that thread, so blocking is fine there.
pub trait Fetch: Send {
    fn fetch(&mut self, req: &FetchRequest) -> Result<SeriesRows, AdapterError>;

    /// Identities this source can name, for typeahead. `None` when the
    /// source cannot enumerate (a REST endpoint). Called once at open and
    /// on `Request::Identities`.
    fn catalogue(&mut self) -> Option<Vec<String>>;
}
```

Add to the `Adapter` trait, after `egress`:

```rust
    /// The on-demand side, or `None` if this adapter has none. Defaulted,
    /// so the vendor adapters built outside this repository against the
    /// two-door contract keep compiling.
    fn fetch(&self) -> Option<Box<dyn Fetch>> {
        None
    }
```

and change the trait's doc phrase "Both capability doors return `Option`" to "All three capability doors return `Option`".

- [ ] **Step 4: Run the tests**

Run: `cargo test -p geode-data adapter::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-data/src/adapter/mod.rs
git commit -m "data: the Fetch adapter shape — FetchRequest, SeriesRows, Adapter::fetch"
```

---
### Task 4: Series storage — DDL and `append_series`

**Files:**
- Create: `crates/geode-data/src/store/series.rs`
- Modify: `crates/geode-data/src/store/mod.rs` (`pub mod series;`, the `apply_schema` arm at ~113), `crates/geode-data/src/store/ddl.rs` (`table_pairs` at 98, `tests_support` at 454)
- Test: `crates/geode-data/src/store/series.rs` (`mod tests`)

**Interfaces:**
- Produces, in `geode_data::store::series`:
```rust
pub const STAGING_TABLE: &str = "staging_series";
pub fn series_table(dataset: &str) -> String;            // "{dataset}_series"
pub fn coverage_table(dataset: &str) -> String;          // "{dataset}_series_coverage"
pub fn create_series_tables_sql(ds: &DatasetSpec) -> Vec<String>;
pub type Span = (DateTime<Utc>, DateTime<Utc>);
pub struct SeriesAppendRequest<'a> { pub dataset: &'a DatasetSpec, pub source: &'a str, pub identity: &'a str, pub rows: &'a SeriesRows, pub span: Span, pub received_at: DateTime<Utc> }
#[derive(Debug, PartialEq, Eq)] pub struct SeriesAppended { pub appended: usize, pub swept: usize }
pub fn append_series(store: &Store, req: &SeriesAppendRequest) -> Result<SeriesAppended, StoreError>;
pub fn micros(t: DateTime<Utc>) -> i64;  pub fn from_micros(us: i64) -> DateTime<Utc>;
```
- `ddl::table_pairs` answers an empty `Vec` for a series dataset (no live/archive pair exists), and `apply_schema` runs `create_series_tables_sql` for it. `tests_support` gains `series_dataset() -> DatasetSpec` and `series_rows(start: &str, minutes: usize, first: f64) -> SeriesRows`.

- [ ] **Step 1: Write the failing tests** (`mod tests` at the bottom of the new `series.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use crate::store::catalog::Catalog;
    use crate::store::ddl::tests_support::{series_dataset, series_rows, ts};

    fn fixture() -> (tempfile::TempDir, Store, DatasetSpec) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        let ds = series_dataset();
        store.apply_schema(&ds).unwrap();
        Catalog::new(store.writer()).ensure_tables().unwrap();
        (dir, store, ds)
    }

    fn append(store: &Store, ds: &DatasetSpec, rows: &SeriesRows, span: (&str, &str), at: &str) -> SeriesAppended {
        append_series(
            store,
            &SeriesAppendRequest {
                dataset: ds,
                source: "demo_kdb",
                identity: "SPX.close",
                rows,
                span: (ts(span.0), ts(span.1)),
                received_at: ts(at),
            },
        )
        .unwrap()
    }

    /// `(epoch micros of ts, epoch micros of received_at, value)` for the
    /// pair, oldest first, every version — the raw table, not the live view.
    fn all_rows(store: &Store) -> Vec<(i64, i64, f64)> {
        let mut stmt = store
            .writer()
            .prepare(
                "select epoch_us(ts), epoch_us(received_at), value from series_series \
                 where source = 'demo_kdb' and series_id = 'SPX.close' order by ts, received_at",
            )
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    #[test]
    fn a_first_append_lands_every_row_stamped_with_received_at() {
        let (_d, store, ds) = fixture();
        let rows = series_rows("2026-01-05T14:30:00Z", 3, 100.0);
        let out = append(&store, &ds, &rows, ("2026-01-05T00:00:00Z", "2026-01-06T00:00:00Z"), "2026-01-06T09:00:00Z");
        assert_eq!(out, SeriesAppended { appended: 3, swept: 0 });
        let got = all_rows(&store);
        assert_eq!(got.len(), 3);
        assert_eq!(got[0], (micros(ts("2026-01-05T14:30:00Z")), micros(ts("2026-01-06T09:00:00Z")), 100.0));
        assert_eq!(got[2].0, micros(ts("2026-01-05T14:32:00Z")));
        let cov: (i64, i64, i64) = store
            .writer()
            .query_row(
                "select epoch_us(from_ts), epoch_us(to_ts), count(*) over () from series_series_coverage",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(cov, (micros(ts("2026-01-05T00:00:00Z")), micros(ts("2026-01-06T00:00:00Z")), 1));
    }

    #[test]
    fn an_overlapping_refetch_with_the_same_values_appends_nothing_but_records_coverage() {
        let (_d, store, ds) = fixture();
        let rows = series_rows("2026-01-05T14:30:00Z", 3, 100.0);
        append(&store, &ds, &rows, ("2026-01-05T00:00:00Z", "2026-01-06T00:00:00Z"), "2026-01-06T09:00:00Z");
        let again = append(&store, &ds, &rows, ("2026-01-05T00:00:00Z", "2026-01-06T00:00:00Z"), "2026-01-07T09:00:00Z");
        assert_eq!(again.appended, 0);
        assert_eq!(all_rows(&store).len(), 3, "the table did not grow");
        let n: i64 = store
            .writer()
            .query_row("select count(*) from series_series_coverage", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2, "coverage is recorded even when nothing was new");
    }

    #[test]
    fn a_corrected_value_is_one_more_row_and_the_older_one_survives() {
        let (_d, store, ds) = fixture();
        let rows = series_rows("2026-01-05T14:30:00Z", 1, 100.0);
        append(&store, &ds, &rows, ("2026-01-05T00:00:00Z", "2026-01-06T00:00:00Z"), "2026-01-06T09:00:00Z");
        let corrected = series_rows("2026-01-05T14:30:00Z", 1, 101.0);
        let out = append(&store, &ds, &corrected, ("2026-01-05T00:00:00Z", "2026-01-06T00:00:00Z"), "2026-01-07T09:00:00Z");
        assert_eq!(out.appended, 1);
        let got = all_rows(&store);
        assert_eq!(got.len(), 2);
        assert_eq!((got[0].2, got[1].2), (100.0, 101.0));
        assert!(got[1].1 > got[0].1, "the correction has the later received_at");
    }

    #[test]
    fn an_empty_fetch_records_coverage_and_appends_nothing() {
        let (_d, store, ds) = fixture();
        let out = append(&store, &ds, &SeriesRows::default(), ("2026-01-03T00:00:00Z", "2026-01-04T00:00:00Z"), "2026-01-06T09:00:00Z");
        assert_eq!(out, SeriesAppended { appended: 0, swept: 0 });
        let n: i64 = store
            .writer()
            .query_row("select count(*) from series_series_coverage", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn invalid_rows_are_refused_before_anything_is_written() {
        let (_d, store, ds) = fixture();
        let bad = SeriesRows { ts: vec![ts("2026-01-05T14:30:00Z")], value: vec![1.0, 2.0] };
        let err = append_series(
            &store,
            &SeriesAppendRequest { dataset: &ds, source: "demo_kdb", identity: "SPX.close", rows: &bad,
                span: (ts("2026-01-05T00:00:00Z"), ts("2026-01-06T00:00:00Z")), received_at: ts("2026-01-06T09:00:00Z") },
        )
        .unwrap_err();
        assert!(matches!(err, StoreError::Series(_)), "{err}");
        assert!(all_rows(&store).is_empty());
        let n: i64 = store.writer().query_row("select count(*) from series_series_coverage", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn micros_round_trip() {
        let t = ts("2026-01-05T14:30:00Z");
        assert_eq!(from_micros(micros(t)), t);
    }
}
```

Add to `ddl.rs`'s `tests_support`:

```rust
    /// The timeseries spec's one series dataset (§4.2), parsed through
    /// the real reader for the same reason `cvi_dataset` is.
    pub(crate) fn series_dataset() -> DatasetSpec {
        let text = "[series]\nfamily = \"series\"\nretention = \"30d\"\nhistory = \"5y\"\n";
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        SchemaSpec::from_doc(&doc).0.dataset("series").unwrap().clone()
    }

    /// `minutes` one-minute bars from `start`, values `first`, `first + 1`, …
    pub(crate) fn series_rows(start: &str, minutes: usize, first: f64) -> crate::adapter::SeriesRows {
        let start = ts(start);
        crate::adapter::SeriesRows {
            ts: (0..minutes).map(|i| start + chrono::Duration::minutes(i as i64)).collect(),
            value: (0..minutes).map(|i| first + i as f64).collect(),
        }
    }
```

Add a test to `ddl.rs`'s own `mod tests`:

```rust
    #[test]
    fn a_series_dataset_has_no_live_archive_pair() {
        let ds = tests_support::series_dataset();
        assert!(table_pairs(&ds).is_empty());
        assert!(history_of("series", &ds).is_empty());
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-data store::series`
Expected: compile error, module `series` not found.

- [ ] **Step 3: Implement `store/series.rs`**

```rust
//! The series family's storage (timeseries spec §4.4–§4.7): one
//! bitemporal, append-only table per dataset and a coverage table beside
//! it. There is no live/archive pair and no generation: "live" is the
//! latest `received_at` per `(source, series_id, ts)`, and as-of is a
//! filter on `received_at`. `append_series` is the ONE door rows enter
//! by, whatever produced them — a fetch today, a tail later.
//!
//! Both timestamps are UTC stored as naive `TIMESTAMP`, bound and read as
//! epoch microseconds (`make_timestamp` / `epoch_us`), so no session time
//! zone can shift them. The staging table is the fixed global
//! `staging_series` — one writer, the ingest thread — for the reason
//! `docs/ingest-cold-start-handoff.md` records.

use crate::adapter::SeriesRows;
use crate::store::{Store, StoreError};
use chrono::{DateTime, Utc};
use duckdb::Connection;
use geode_core::schema::{DatasetSpec, SERIES_COLUMNS};

pub const STAGING_TABLE: &str = "staging_series";

pub type Span = (DateTime<Utc>, DateTime<Utc>);

pub fn series_table(dataset: &str) -> String {
    format!("{dataset}_series")
}

pub fn coverage_table(dataset: &str) -> String {
    format!("{dataset}_series_coverage")
}

pub fn micros(t: DateTime<Utc>) -> i64 {
    t.timestamp_micros()
}

pub fn from_micros(us: i64) -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp_micros(us).expect("a stored timestamp is in range")
}

/// The two `CREATE TABLE IF NOT EXISTS` statements a series dataset
/// owns. The column order is `SERIES_COLUMNS`, the one list every reader
/// and writer of this family shares.
pub fn create_series_tables_sql(ds: &DatasetSpec) -> Vec<String> {
    debug_assert_eq!(ds.series_columns(), &SERIES_COLUMNS);
    vec![
        format!(
            "CREATE TABLE IF NOT EXISTS {} (\n  \"source\" VARCHAR,\n  \"series_id\" VARCHAR,\n  \
             \"ts\" TIMESTAMP,\n  \"received_at\" TIMESTAMP,\n  \"value\" DOUBLE,\n  \
             PRIMARY KEY (\"source\", \"series_id\", \"ts\", \"received_at\")\n);",
            series_table(&ds.name)
        ),
        format!(
            "CREATE TABLE IF NOT EXISTS {} (\n  \"source\" VARCHAR,\n  \"series_id\" VARCHAR,\n  \
             \"from_ts\" TIMESTAMP,\n  \"to_ts\" TIMESTAMP,\n  \"received_at\" TIMESTAMP\n);",
            coverage_table(&ds.name)
        ),
    ]
}

pub struct SeriesAppendRequest<'a> {
    pub dataset: &'a DatasetSpec,
    pub source: &'a str,
    pub identity: &'a str,
    pub rows: &'a SeriesRows,
    /// The half-open span this fetch covered, recorded whether or not any
    /// row was new — an empty gap is not asked for again.
    pub span: Span,
    pub received_at: DateTime<Utc>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct SeriesAppended {
    pub appended: usize,
    /// Rows the per-pair retention sweep (§4.7) deleted in the same
    /// transaction.
    pub swept: usize,
}

fn sql_err(statement: &str) -> impl FnOnce(duckdb::Error) -> StoreError + '_ {
    move |source| StoreError::Sql {
        statement: statement.to_string(),
        source,
    }
}

pub fn append_series(
    store: &Store,
    req: &SeriesAppendRequest,
) -> Result<SeriesAppended, StoreError> {
    // Before anything is written: malformed rows leave the store exactly
    // as it was, coverage included.
    req.rows
        .validate()
        .map_err(|e| StoreError::Series(e.message))?;
    let conn = store.writer();
    let table = series_table(&req.dataset.name);
    let coverage = coverage_table(&req.dataset.name);

    // 1. Stage as BIGINT micros: position lines the insert up, and a
    // micros column cannot be bent by a session time zone.
    let create = format!(
        "create or replace table {STAGING_TABLE} (\"ts_us\" BIGINT, \"value\" DOUBLE)"
    );
    conn.execute_batch(&create).map_err(sql_err(&create))?;
    {
        let mut app = conn
            .appender(STAGING_TABLE)
            .map_err(sql_err("appender on staging_series"))?;
        for i in 0..req.rows.len() {
            app.append_row(duckdb::params![micros(req.rows.ts[i]), req.rows.value[i]])
                .map_err(sql_err("append row into staging_series"))?;
        }
        app.flush().map_err(sql_err("flush staging_series"))?;
    }

    conn.execute_batch("begin;").map_err(sql_err("begin"))?;
    match append_in_transaction(conn, req, &table, &coverage) {
        Ok(out) => {
            conn.execute_batch("commit;").map_err(sql_err("commit"))?;
            Ok(out)
        }
        Err(e) => {
            let _ = conn.execute_batch("rollback;");
            Err(e)
        }
    }
}

fn append_in_transaction(
    conn: &Connection,
    req: &SeriesAppendRequest,
    table: &str,
    coverage: &str,
) -> Result<SeriesAppended, StoreError> {
    let received = micros(req.received_at);
    // 2. Drop a staged row equal to the LIVE value for its ts, so an
    // overlapping refetch grows nothing. Live is the greatest
    // received_at per ts, expressed inline.
    let dedupe = format!(
        "delete from {STAGING_TABLE} s where exists (
             select 1 from (
                 select ts, arg_max(value, received_at) as v from {table}
                 where source = ? and series_id = ? group by ts
             ) live
             where live.ts = make_timestamp(s.ts_us) and live.v = s.value
         )"
    );
    conn.execute(&dedupe, duckdb::params![req.source, req.identity])
        .map_err(sql_err(&dedupe))?;
    // 3. Insert what remains under one received_at.
    let insert = format!(
        "insert into {table} (source, series_id, ts, received_at, value)
         select ?, ?, make_timestamp(ts_us), make_timestamp(?), value from {STAGING_TABLE}"
    );
    let appended = conn
        .execute(&insert, duckdb::params![req.source, req.identity, received])
        .map_err(sql_err(&insert))?;
    // 4. Coverage, always.
    let cover = format!(
        "insert into {coverage} (source, series_id, from_ts, to_ts, received_at)
         values (?, ?, make_timestamp(?), make_timestamp(?), make_timestamp(?))"
    );
    conn.execute(
        &cover,
        duckdb::params![req.source, req.identity, micros(req.span.0), micros(req.span.1), received],
    )
    .map_err(sql_err(&cover))?;
    // 5. Retention for this pair (Task 6 fills this in; 0 until then).
    let swept = 0;
    Ok(SeriesAppended { appended, swept })
}
```

Add a `Series(String)` variant to `StoreError` in `store/mod.rs` with a `Display` arm `StoreError::Series(reason) => write!(f, "series: {reason}")`, mirroring `Document`.

In `store/mod.rs` add `pub mod series;` and change `apply_schema`:

```rust
    pub fn apply_schema(&self, ds: &DatasetSpec) -> Result<(), StoreError> {
        if ds.is_series() {
            // No live/archive pair: one table plus its coverage table
            // (timeseries spec §4.4), created once for both "kinds".
            for sql in series::create_series_tables_sql(ds) {
                self.writer
                    .execute_batch(&sql)
                    .map_err(|source| StoreError::Sql { statement: sql, source })?;
            }
            return Ok(());
        }
        for kind in [TableKind::Live, TableKind::Archive] {
            …unchanged…
```

In `ddl.rs::table_pairs`:

```rust
pub fn table_pairs(ds: &DatasetSpec) -> Vec<TablePair> {
    if ds.is_series() {
        // A series dataset has no live/archive pair at all (timeseries
        // spec §4.4): nothing to sweep by generation, nothing to
        // reconcile, nothing in `history_of`. `store::series` names its
        // tables.
        return Vec::new();
    }
    if ds.is_document() {
        …
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p geode-data store::`
Expected: the six new series tests and the ddl test pass; every existing store test passes.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-data/src/store
git commit -m "data: series storage — DDL, append_series, coverage rows"
```

---

### Task 5: Coverage and `missing_spans`

**Files:**
- Modify: `crates/geode-data/src/store/series.rs`
- Test: same file

**Interfaces:**
- Produces:
```rust
/// Merged, sorted, disjoint spans loaded for the pair, from the coverage table.
pub fn coverage(conn: &Connection, dataset: &str, source: &str, identity: &str) -> Result<Vec<Span>, StoreError>;
/// Pure: `requested` minus the union of `loaded`; sorted, disjoint, half-open.
pub fn missing_spans(requested: Span, loaded: &[Span]) -> Vec<Span>;
/// Pure: sort and merge overlapping or touching spans.
pub fn merge_spans(spans: &mut Vec<Span>);
```

- [ ] **Step 1: Write the failing tests** (add to `series.rs`'s `mod tests`; add `use proptest::prelude::*;`)

```rust
    fn sp(a: &str, b: &str) -> Span {
        (ts(a), ts(b))
    }

    #[test]
    fn missing_spans_subtracts_loaded_spans() {
        let req = sp("2026-01-01T00:00:00Z", "2026-01-10T00:00:00Z");
        assert_eq!(missing_spans(req, &[]), vec![req]);
        assert_eq!(missing_spans(req, &[req]), vec![]);
        let loaded = [sp("2026-01-03T00:00:00Z", "2026-01-05T00:00:00Z")];
        assert_eq!(
            missing_spans(req, &loaded),
            vec![sp("2026-01-01T00:00:00Z", "2026-01-03T00:00:00Z"), sp("2026-01-05T00:00:00Z", "2026-01-10T00:00:00Z")]
        );
        // A loaded span wider than the request leaves nothing.
        assert_eq!(missing_spans(req, &[sp("2025-12-01T00:00:00Z", "2026-02-01T00:00:00Z")]), vec![]);
        // Touching spans merge: [1,3) and [3,5) leave [5,10).
        assert_eq!(
            missing_spans(req, &[sp("2026-01-01T00:00:00Z", "2026-01-03T00:00:00Z"), sp("2026-01-03T00:00:00Z", "2026-01-05T00:00:00Z")]),
            vec![sp("2026-01-05T00:00:00Z", "2026-01-10T00:00:00Z")]
        );
        // An empty request is nothing.
        assert_eq!(missing_spans(sp("2026-01-05T00:00:00Z", "2026-01-05T00:00:00Z"), &[]), vec![]);
    }

    #[test]
    fn coverage_reads_back_merged_spans() {
        let (_d, store, ds) = fixture();
        let rows = SeriesRows::default();
        append(&store, &ds, &rows, ("2026-01-01T00:00:00Z", "2026-01-03T00:00:00Z"), "2026-01-06T09:00:00Z");
        append(&store, &ds, &rows, ("2026-01-02T00:00:00Z", "2026-01-05T00:00:00Z"), "2026-01-06T09:01:00Z");
        append(&store, &ds, &rows, ("2026-01-08T00:00:00Z", "2026-01-09T00:00:00Z"), "2026-01-06T09:02:00Z");
        let got = coverage(store.writer(), "series", "demo_kdb", "SPX.close").unwrap();
        assert_eq!(got, vec![sp("2026-01-01T00:00:00Z", "2026-01-05T00:00:00Z"), sp("2026-01-08T00:00:00Z", "2026-01-09T00:00:00Z")]);
        assert!(coverage(store.writer(), "series", "demo_kdb", "VIX").unwrap().is_empty());
    }

    proptest! {
        /// loaded ∪ missing covers requested exactly, and missing is
        /// disjoint from loaded.
        #[test]
        fn missing_and_loaded_partition_the_request(
            req_from in 0i64..1000, req_len in 0i64..1000,
            loaded in prop::collection::vec((0i64..1000, 1i64..300), 0..6),
        ) {
            let base = ts("2026-01-01T00:00:00Z");
            let at = |h: i64| base + chrono::Duration::hours(h);
            let req = (at(req_from), at(req_from + req_len));
            let loaded: Vec<Span> = loaded.iter().map(|(f, l)| (at(*f), at(f + l))).collect();
            let missing = missing_spans(req, &loaded);
            for h in req_from..req_from + req_len {
                let t = at(h);
                let in_loaded = loaded.iter().any(|(f, to)| *f <= t && t < *to);
                let in_missing = missing.iter().any(|(f, to)| *f <= t && t < *to);
                prop_assert_ne!(in_loaded, in_missing, "hour {h} must be in exactly one");
            }
            for w in missing.windows(2) {
                prop_assert!(w[0].1 <= w[1].0, "missing spans are sorted and disjoint");
            }
        }
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-data store::series::tests::missing_spans_subtracts_loaded_spans`
Expected: compile error, `missing_spans` not found.

- [ ] **Step 3: Implement** (in `series.rs`)

```rust
/// Sorts `spans` and merges any that overlap or touch, in place.
pub fn merge_spans(spans: &mut Vec<Span>) {
    spans.retain(|(f, t)| f < t);
    spans.sort();
    let mut out: Vec<Span> = Vec::with_capacity(spans.len());
    for &(f, t) in spans.iter() {
        match out.last_mut() {
            Some(last) if f <= last.1 => last.1 = last.1.max(t),
            _ => out.push((f, t)),
        }
    }
    *spans = out;
}

/// `requested` minus the union of `loaded`: the spans a fetch still has
/// to ask the source for. Pure, so the service thread can run it over the
/// coverage rows it read (timeseries spec §4.6).
pub fn missing_spans(requested: Span, loaded: &[Span]) -> Vec<Span> {
    let (from, to) = requested;
    if from >= to {
        return Vec::new();
    }
    let mut loaded = loaded.to_vec();
    merge_spans(&mut loaded);
    let mut out = Vec::new();
    let mut cursor = from;
    for (f, t) in loaded {
        if t <= cursor {
            continue;
        }
        if f >= to {
            break;
        }
        if f > cursor {
            out.push((cursor, f));
        }
        cursor = cursor.max(t);
        if cursor >= to {
            return out;
        }
    }
    if cursor < to {
        out.push((cursor, to));
    }
    out
}

/// The pair's loaded spans, merged. A coverage row is written per fetch
/// (`append_series` step 4), so this is the union of every fetch so far.
pub fn coverage(
    conn: &Connection,
    dataset: &str,
    source: &str,
    identity: &str,
) -> Result<Vec<Span>, StoreError> {
    let sql = format!(
        "select epoch_us(from_ts), epoch_us(to_ts) from {} where source = ? and series_id = ? \
         order by from_ts",
        coverage_table(dataset)
    );
    let mut stmt = conn.prepare(&sql).map_err(sql_err(&sql))?;
    let rows = stmt
        .query_map(duckdb::params![source, identity], |r| {
            Ok((from_micros(r.get::<_, i64>(0)?), from_micros(r.get::<_, i64>(1)?)))
        })
        .map_err(sql_err(&sql))?;
    let mut spans = Vec::new();
    for row in rows {
        spans.push(row.map_err(sql_err(&sql))?);
    }
    merge_spans(&mut spans);
    Ok(spans)
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p geode-data store::series`
Expected: PASS, including the property test.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-data/src/store/series.rs
git commit -m "data: series coverage read-back and missing_spans"
```

---

### Task 6: Per-pair retention

Spec §4.7 says retention "runs in the existing sweeper". There is no production sweeper: `store::retention::sweep` has no caller outside tests. So per-pair retention runs inside `append_series` (step 5), scoped to the pair just written, which bounds growth exactly where it happens and costs one indexed delete. Task 10 amends the spec to say so.

**Files:**
- Modify: `crates/geode-data/src/store/series.rs`
- Test: same file

**Interfaces:**
- Produces: `pub fn sweep_pair(conn: &Connection, ds: &DatasetSpec, source: &str, identity: &str, now: DateTime<Utc>) -> Result<usize, StoreError>` (rows deleted); `append_in_transaction` calls it with `now = req.received_at`.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn retention_deletes_superseded_rows_older_than_the_window_and_keeps_live() {
        let (_d, store, ds) = fixture(); // retention = 30d, history = 5y
        let old = series_rows("2026-01-05T14:30:00Z", 1, 100.0);
        append(&store, &ds, &old, ("2026-01-05T00:00:00Z", "2026-01-06T00:00:00Z"), "2026-01-06T09:00:00Z");
        let corrected = series_rows("2026-01-05T14:30:00Z", 1, 101.0);
        // 10 days later: the superseded row is inside the 30d window, kept.
        let out = append(&store, &ds, &corrected, ("2026-01-05T00:00:00Z", "2026-01-06T00:00:00Z"), "2026-01-16T09:00:00Z");
        assert_eq!(out.swept, 0);
        assert_eq!(all_rows(&store).len(), 2);
        // 40 days after the first: the superseded row is outside it, swept;
        // the live row (101.0) survives whatever its age.
        let untouched = series_rows("2026-01-05T14:31:00Z", 1, 5.0);
        let out = append(&store, &ds, &untouched, ("2026-01-05T00:00:00Z", "2026-01-06T00:00:00Z"), "2026-02-20T09:00:00Z");
        assert_eq!(out.swept, 1);
        let got = all_rows(&store);
        assert_eq!(got.iter().map(|r| r.2).collect::<Vec<_>>(), vec![101.0, 5.0]);
    }

    #[test]
    fn history_deletes_rows_and_coverage_whose_ts_is_too_old() {
        let (_d, store, ds) = fixture(); // history = 5y
        let ancient = series_rows("2019-01-05T14:30:00Z", 2, 1.0);
        append(&store, &ds, &ancient, ("2019-01-05T00:00:00Z", "2019-01-06T00:00:00Z"), "2019-01-06T09:00:00Z");
        let recent = series_rows("2026-01-05T14:30:00Z", 1, 100.0);
        let out = append(&store, &ds, &recent, ("2026-01-05T00:00:00Z", "2026-01-06T00:00:00Z"), "2026-01-06T09:00:00Z");
        assert_eq!(out.swept, 2);
        assert_eq!(all_rows(&store).len(), 1);
        let cov = coverage(store.writer(), "series", "demo_kdb", "SPX.close").unwrap();
        assert_eq!(cov, vec![sp("2026-01-05T00:00:00Z", "2026-01-06T00:00:00Z")], "the ancient coverage row went with its rows");
    }

    #[test]
    fn an_unbounded_policy_sweeps_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        let text = "[series]\nfamily = \"series\"\n";
        let doc = geode_core::config::merge_docs("datasets", &[geode_core::config::LayerDoc::builtin("datasets", text).unwrap()]);
        let ds = geode_core::schema::SchemaSpec::from_doc(&doc).0.dataset("series").unwrap().clone();
        store.apply_schema(&ds).unwrap();
        let a = series_rows("2019-01-05T14:30:00Z", 1, 1.0);
        append(&store, &ds, &a, ("2019-01-05T00:00:00Z", "2019-01-06T00:00:00Z"), "2019-01-06T09:00:00Z");
        let b = series_rows("2019-01-05T14:30:00Z", 1, 2.0);
        let out = append(&store, &ds, &b, ("2019-01-05T00:00:00Z", "2019-01-06T00:00:00Z"), "2030-01-06T09:00:00Z");
        assert_eq!(out.swept, 0);
        assert_eq!(all_rows(&store).len(), 2);
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-data store::series::tests::retention_deletes_superseded_rows_older_than_the_window_and_keeps_live`
Expected: FAIL, `assertion left == right` on `swept` (0 vs 1).

- [ ] **Step 3: Implement**

```rust
/// The series family's retention (timeseries spec §4.7), for ONE pair,
/// run inside `append_series`'s transaction with `now` = the append's
/// `received_at`. `retention` deletes rows that are superseded (a later
/// `received_at` exists for the same ts) and older than the window; the
/// live row survives whatever its age. `history` deletes rows and
/// coverage whose `ts`/`to_ts` are older than the window.
pub fn sweep_pair(
    conn: &Connection,
    ds: &DatasetSpec,
    source: &str,
    identity: &str,
    now: DateTime<Utc>,
) -> Result<usize, StoreError> {
    let Some(policy) = ds.series_retention else {
        return Ok(0);
    };
    let table = series_table(&ds.name);
    let mut swept = 0;
    if let Some(window) = policy.retention {
        let cutoff = micros(now) - window.as_micros() as i64;
        let sql = format!(
            "delete from {table} t where source = ? and series_id = ? \
             and epoch_us(received_at) < ? \
             and exists (select 1 from {table} n where n.source = t.source \
                 and n.series_id = t.series_id and n.ts = t.ts and n.received_at > t.received_at)"
        );
        swept += conn
            .execute(&sql, duckdb::params![source, identity, cutoff])
            .map_err(sql_err(&sql))?;
    }
    if let Some(window) = policy.history {
        let cutoff = micros(now) - window.as_micros() as i64;
        let sql = format!(
            "delete from {table} where source = ? and series_id = ? and epoch_us(ts) < ?"
        );
        swept += conn
            .execute(&sql, duckdb::params![source, identity, cutoff])
            .map_err(sql_err(&sql))?;
        let sql = format!(
            "delete from {} where source = ? and series_id = ? and epoch_us(to_ts) <= ?",
            coverage_table(&ds.name)
        );
        conn.execute(&sql, duckdb::params![source, identity, cutoff])
            .map_err(sql_err(&sql))?;
    }
    Ok(swept)
}
```

and in `append_in_transaction`, replace `let swept = 0;` with:

```rust
    // 5. Retention for this pair, in the same transaction (§4.7): the
    // one place growth happens is the one place it is bounded.
    let swept = sweep_pair(conn, req.dataset, req.source, req.identity, req.received_at)?;
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p geode-data store::series`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-data/src/store/series.rs
git commit -m "data: per-pair series retention inside append_series"
```

---
### Task 7: The ingest runner's third lane

**Files:**
- Modify: `crates/geode-data/src/ingest/runner.rs` (`IngestEvent` at 60, `DocumentJob`/`Queue` at 110–155, `IngestHandle` impl at 236–270, `Work`/`take_work` at 351–393, the `match work` in `run` at 558–575, `publish_one_document` at 395 as the template)
- Modify: `crates/geode-data/src/ingest/mod.rs` (re-export `SeriesJob`)
- Test: `crates/geode-data/src/ingest/runner.rs` (`mod tests`)

**Interfaces:**
- Produces:
```rust
pub struct SeriesJob { pub source: String, pub dataset: String, pub identity: String, pub rows: SeriesRows, pub span: Span, pub received_at: DateTime<Utc> }
IngestEvent::SeriesAppended { source: String, dataset: String, identity: String, appended: usize, swept: usize }
IngestEvent::SeriesFailed  { source: String, dataset: String, identity: String, reason: String }
impl IngestHandle { pub fn submit_series(&self, job: SeriesJob); }
```
- `Started` is emitted for a series job with `path = "series://{source}/{identity}"`, and `take_work` pops documents, then series, then files.

- [ ] **Step 1: Write the failing tests** (in `runner.rs`'s `mod tests`; find the existing document-lane test that uses `IngestRunner::spawn_channel` and the `cvi_dataset` fixture as the pattern)

```rust
    fn series_store() -> (tempfile::TempDir, Store, SchemaSpec) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        let ds = crate::store::ddl::tests_support::series_dataset();
        store.apply_schema(&ds).unwrap();
        crate::store::catalog::Catalog::new(store.writer()).ensure_tables().unwrap();
        let mut schema = SchemaSpec::default();
        schema.datasets.push(ds);
        (dir, store, schema)
    }

    fn series_job(identity: &str, rows: SeriesRows) -> SeriesJob {
        SeriesJob {
            source: "demo_kdb".into(),
            dataset: "series".into(),
            identity: identity.into(),
            rows,
            span: (ts("2026-01-05T00:00:00Z"), ts("2026-01-06T00:00:00Z")),
            received_at: ts("2026-01-06T09:00:00Z"),
        }
    }

    #[test]
    fn a_series_job_is_appended_and_announced() {
        let (_d, store, schema) = series_store();
        let (handle, rx) = IngestRunner::spawn_channel(store, schema);
        handle.submit_series(series_job("SPX.close", series_rows("2026-01-05T14:30:00Z", 3, 100.0)));
        let started = rx.recv_timeout(Duration::from_secs(30)).unwrap();
        assert!(matches!(&started, IngestEvent::Started { source, path, .. }
            if source == "demo_kdb" && path == "series://demo_kdb/SPX.close"), "{started:?}");
        let done = rx.recv_timeout(Duration::from_secs(30)).unwrap();
        assert!(matches!(&done, IngestEvent::SeriesAppended { source, dataset, identity, appended: 3, swept: 0 }
            if source == "demo_kdb" && dataset == "series" && identity == "SPX.close"), "{done:?}");
        handle.shutdown();
    }

    #[test]
    fn a_series_job_for_an_undeclared_dataset_fails_by_name() {
        let (_d, store, schema) = series_store();
        let (handle, rx) = IngestRunner::spawn_channel(store, schema);
        let mut job = series_job("SPX.close", SeriesRows::default());
        job.dataset = "nope".into();
        handle.submit_series(job);
        let done = loop {
            match rx.recv_timeout(Duration::from_secs(30)).unwrap() {
                IngestEvent::SeriesFailed { reason, identity, .. } => break (reason, identity),
                _ => continue,
            }
        };
        assert_eq!(done.1, "SPX.close");
        assert!(done.0.contains("dataset 'nope' is not declared"), "{}", done.0);
        handle.shutdown();
    }

    #[test]
    fn invalid_series_rows_fail_the_job_and_leave_the_runner_working() {
        let (_d, store, schema) = series_store();
        let (handle, rx) = IngestRunner::spawn_channel(store, schema);
        let bad = SeriesRows { ts: vec![ts("2026-01-05T14:30:00Z")], value: vec![1.0, 2.0] };
        handle.submit_series(series_job("SPX.close", bad));
        handle.submit_series(series_job("VIX", series_rows("2026-01-05T14:30:00Z", 1, 20.0)));
        let mut failed = None;
        let mut appended = None;
        for _ in 0..6 {
            match rx.recv_timeout(Duration::from_secs(30)).unwrap() {
                IngestEvent::SeriesFailed { identity, reason, .. } => failed = Some((identity, reason)),
                IngestEvent::SeriesAppended { identity, appended: n, .. } => appended = Some((identity, n)),
                _ => {}
            }
            if failed.is_some() && appended.is_some() { break; }
        }
        let (id, reason) = failed.unwrap();
        assert_eq!(id, "SPX.close");
        assert!(reason.contains("2 values for 1 timestamps"), "{reason}");
        assert_eq!(appended.unwrap(), ("VIX".to_string(), 1));
        handle.shutdown();
    }

    #[test]
    fn take_work_pops_documents_then_series_then_files() {
        let mut q = Queue::default();
        q.series.push_back(series_job("SPX.close", SeriesRows::default()));
        q.items.push(work_item_for_tests("a.csv")); // reuse the existing file-item fixture in this module
        let first = take_work(&mut q).unwrap();
        assert!(matches!(first, Work::Series(_)));
        let second = take_work(&mut q).unwrap();
        assert!(matches!(second, Work::File(_)));
        assert!(take_work(&mut q).is_none());
    }
```

Import `crate::store::ddl::tests_support::{series_rows, ts}` and `crate::adapter::SeriesRows` in the tests module if they are not already there. If this module has no file-item fixture named `work_item_for_tests`, build one the way the existing `take_work` tests do (search the module for `Queue::default()` and copy that test's `WorkItem` construction).

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-data ingest::runner::tests::a_series_job_is_appended_and_announced`
Expected: compile error, `SeriesJob` / `submit_series` not found.

- [ ] **Step 3: Implement**

Add the two `IngestEvent` variants after `Failed`:

```rust
    /// A series job appended (timeseries spec §5.4 step 3). `appended`
    /// may be 0 — an overlapping refetch — and the event is still sent,
    /// because coverage was recorded and the asking tile must requery.
    SeriesAppended {
        source: String,
        dataset: String,
        identity: String,
        appended: usize,
        swept: usize,
    },
    /// A series job that could not be appended: refused rows, an
    /// undeclared dataset, or a panic inside the append. Load lane,
    /// keyed by the pair.
    SeriesFailed {
        source: String,
        dataset: String,
        identity: String,
        reason: String,
    },
```

Add `SeriesJob` after `DocumentJob`:

```rust
/// Fetched rows waiting to append (timeseries spec §5.4). Owned, like
/// `DocumentJob`'s rows, for the same reason.
#[derive(Debug)]
pub struct SeriesJob {
    pub source: String,
    pub dataset: String,
    pub identity: String,
    pub rows: SeriesRows,
    pub span: Span,
    pub received_at: DateTime<Utc>,
}
```

with `use crate::adapter::SeriesRows; use crate::store::series::{Span, SeriesAppendRequest, SeriesAppended, append_series};` at the top.

Add to `Queue` after `documents`:

```rust
    /// Fetched series, taken after documents and ahead of files: a
    /// fetch was asked for by a trader watching a chart, a file was
    /// found by a poll.
    series: VecDeque<SeriesJob>,
```

Add to `impl IngestHandle` after `submit_document`:

```rust
    /// Hand fetched rows to the runner. No dedupe and no refusal, as
    /// `submit_document`: the service subtracted coverage before the
    /// fetch, and `append_series` drops unchanged rows regardless.
    pub fn submit_series(&self, job: SeriesJob) {
        let (lock, cvar) = &*self.queue;
        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
        q.series.push_back(job);
        cvar.notify_all();
    }
```

Extend `Work` and `take_work`:

```rust
enum Work {
    Document(DocumentJob),
    Series(SeriesJob),
    File(WorkItem),
}

fn take_work(q: &mut Queue) -> Option<Work> {
    if let Some(job) = q.documents.pop_front() {
        return Some(Work::Document(job));
    }
    if let Some(job) = q.series.pop_front() {
        return Some(Work::Series(job));
    }
    if q.items.is_empty() {
        return None;
    }
    …unchanged…
}
```

In `run`, the queued count `q.items.len() + q.documents.len()` becomes `q.items.len() + q.documents.len() + q.series.len()`, and the `match work` gains an arm before `Work::File`:

```rust
            Work::Series(job) => {
                if !sink(IngestEvent::Started {
                    source: job.source.clone(),
                    path: format!("series://{}/{}", job.source, job.identity),
                    queued,
                }) {
                    log_refused_event(&refusal_logged, "a load-started announcement");
                }
                append_one_series(&store, &schema, &sink, &refusal_logged, job);
                continue;
            }
```

Add `append_one_series` after `publish_one_document`:

```rust
/// The series arm of the runner (timeseries spec §5.4 step 3), the
/// document arm's shape exactly: resolve the dataset by name, append
/// under `contained`, announce the outcome, count a refused announcement.
fn append_one_series(
    store: &Store,
    schema: &SchemaSpec,
    sink: &IngestSink,
    refusal_logged: &AtomicBool,
    job: SeriesJob,
) {
    let pair = format!("{}@{}", job.identity, job.source);
    let failed = |reason: String| IngestEvent::SeriesFailed {
        source: job.source.clone(),
        dataset: job.dataset.clone(),
        identity: job.identity.clone(),
        reason,
    };
    let Some(dataset) = schema.dataset(&job.dataset) else {
        let event = failed(format!("dataset '{}' is not declared", job.dataset));
        if !sink(event) {
            log_refused_event(refusal_logged, &format!("the undeclared-dataset failure for series {pair}"));
        }
        return;
    };
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        geode_core::panic::contained(|| {
            append_series(
                store,
                &SeriesAppendRequest {
                    dataset,
                    source: &job.source,
                    identity: &job.identity,
                    rows: &job.rows,
                    span: job.span,
                    received_at: job.received_at,
                },
            )
            .map_err(|e| e.to_string())
        })
    }));
    let event = match outcome {
        Ok(Ok(SeriesAppended { appended, swept })) => IngestEvent::SeriesAppended {
            source: job.source.clone(),
            dataset: job.dataset.clone(),
            identity: job.identity.clone(),
            appended,
            swept,
        },
        Ok(Err(reason)) => failed(reason),
        Err(payload) => {
            let message = panic_payload_message(payload.as_ref());
            let path = std::path::PathBuf::from(format!("series://{}/{}", job.source, job.identity));
            log_ingest_panic(&path, &message);
            failed(format!("series append panicked at {}: {message}", path.display()))
        }
    };
    if !sink(event) {
        log_refused_event(refusal_logged, &format!("the series append outcome for {pair}"));
    }
}
```

Re-export in `ingest/mod.rs`: `pub use runner::{DocumentJob, IngestEvent, IngestHandle, IngestRunner, IngestSink, SeriesJob};`. Every existing exhaustive `match` on `IngestEvent` (the service's `ingest_sink`, any test) now fails to compile: add the two arms in `service.rs`'s `ingest_sink` as `IngestEvent::SeriesAppended { .. } | IngestEvent::SeriesFailed { .. } => true` FOR NOW (Task 8 replaces them), and in tests as `_ => continue` where a wildcard already exists.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p geode-data ingest:: && cargo check --workspace --all-targets`
Expected: PASS; workspace compiles.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-data/src/ingest crates/geode-data/src/service.rs
git commit -m "data: the ingest runner's series lane — SeriesJob, SeriesAppended/SeriesFailed"
```

---

### Task 8: `Request::Fetch`, the fetch worker, `SeriesFetched`, catalog rows

**Files:**
- Create: `crates/geode-data/src/ingest/fetch.rs`
- Modify: `crates/geode-data/src/ingest/mod.rs`, `crates/geode-data/src/service.rs` (`DataEvent` at 66, `DataService` fields at 554, `open` at 972–1212, the `ingest_sink` arms from Task 7, `catalog` at 1485), `crates/geode-data/src/handle.rs` (`Request` at 26, `DataHandle` methods, `serve` at 215), `crates/geode-data/src/lib.rs` (re-export `FetchParams`), `crates/geode-data/src/query/catalog.rs` (`dataset_catalog` at 45), `crates/geode-core/src/query.rs` (`DatasetCatalog` at ~160, `CatalogSnapshot` at ~123)
- Modify (literal sites gain `series: Vec::new()` / `identities: Vec::new()`): `crates/geode-shell/src/diagnostics.rs:1213`, `crates/geode-marketdata/src/tile.rs:5431,5450`, `crates/geode-diagnostics/src/sections.rs:797`, and whatever else the compiler names
- Test: `crates/geode-data/src/ingest/fetch.rs` (`mod tests`), `crates/geode-data/src/service.rs` (`mod tests`), `crates/geode-data/src/query/catalog.rs` (`mod tests`)

**Interfaces:**
- `geode_core::query`:
```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeriesCatalog { pub source: String, pub identity: String, pub from: DateTime<Utc>, pub to: DateTime<Utc>, pub fetches: u64, pub latest_received_at: DateTime<Utc> }
// DatasetCatalog gains `pub series: Vec<SeriesCatalog>` (empty on other families).
// CatalogSnapshot gains `pub identities: Vec<(String, Vec<String>)>`  // (source, sorted identities), fetch sources with a catalogue only
```
- `geode_data::ingest::fetch`:
```rust
pub enum FetchWork { Span { identity: String, from: DateTime<Utc>, to: DateTime<Utc> }, Identities }
pub enum FetchOutcome { Fetched { identity: String, rows: SeriesRows, span: Span, dropped: usize }, Failed { identity: String, reason: String }, Identities(Option<Vec<String>>) }
pub type FetchOutcomeSink = Arc<dyn Fn(FetchOutcome) + Send + Sync>;
pub const FETCH_BOUND: usize = 64;
pub struct FetchWorker { … }
impl FetchWorker {
    pub fn spawn(source: &str, fetch: Box<dyn Fetch>, sink: FetchOutcomeSink) -> Result<FetchWorker, AdapterError>;
    pub fn source(&self) -> &str;
    /// `false` when the bounded queue is full or the worker is gone. Never blocks.
    pub fn request(&self, work: FetchWork) -> bool;
    pub fn shutdown(&mut self);   // drops the sender, joins; also on Drop
}
```
- `geode_data::service`:
```rust
DataEvent::SeriesFetched { source: String, identity: String, result: Result<u64, String> }
pub struct FetchParams { pub key: QueryKey, pub source: String, pub identity: String, pub from: DateTime<Utc>, pub to: DateTime<Utc> }
impl DataService { pub fn fetch(&self, params: &FetchParams); pub fn identities(&self, source: &str) -> bool; }
```
- `geode_data::handle`: `Request::Fetch(FetchParams)`, `Request::Identities { source: String }`, `DataHandle::fetch(params) -> bool`, `DataHandle::identities(source: impl Into<String>) -> bool`.

- [ ] **Step 1: Write the failing tests**

`ingest/fetch.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::{AdapterError, Fetch, FetchRequest, SeriesRows};
    use std::sync::Mutex;
    use std::sync::mpsc::channel;

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    /// Answers `n` one-minute bars from `from` for any identity but
    /// "broken", and counts its calls.
    struct FakeFetch { calls: Arc<Mutex<Vec<FetchRequest>>>, n: usize, catalogue: Option<Vec<String>> }
    impl Fetch for FakeFetch {
        fn fetch(&mut self, req: &FetchRequest) -> Result<SeriesRows, AdapterError> {
            self.calls.lock().unwrap().push(req.clone());
            if req.identity == "broken" {
                return Err(AdapterError { message: "no such symbol".into() });
            }
            Ok(SeriesRows {
                ts: (0..self.n).map(|i| req.from + chrono::Duration::minutes(i as i64)).collect(),
                value: (0..self.n).map(|i| if i == 1 { f64::NAN } else { i as f64 }).collect(),
            })
        }
        fn catalogue(&mut self) -> Option<Vec<String>> {
            self.catalogue.clone()
        }
    }

    #[test]
    fn a_span_request_yields_fetched_rows_with_non_finite_values_dropped() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = channel();
        let sink: FetchOutcomeSink = Arc::new(move |o| { let _ = tx.send(o); });
        let mut w = FetchWorker::spawn("demo_kdb", Box::new(FakeFetch { calls: calls.clone(), n: 3, catalogue: None }), sink).unwrap();
        assert!(w.request(FetchWork::Span { identity: "SPX.close".into(), from: ts("2026-01-05T00:00:00Z"), to: ts("2026-01-06T00:00:00Z") }));
        match rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap() {
            FetchOutcome::Fetched { identity, rows, span, dropped } => {
                assert_eq!(identity, "SPX.close");
                assert_eq!(rows.len(), 2);
                assert_eq!(dropped, 1);
                assert_eq!(span, (ts("2026-01-05T00:00:00Z"), ts("2026-01-06T00:00:00Z")));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(calls.lock().unwrap().len(), 1);
        w.shutdown();
    }

    #[test]
    fn an_adapter_error_is_a_failed_outcome_for_that_identity() {
        let (tx, rx) = channel();
        let sink: FetchOutcomeSink = Arc::new(move |o| { let _ = tx.send(o); });
        let mut w = FetchWorker::spawn("demo_kdb", Box::new(FakeFetch { calls: Default::default(), n: 1, catalogue: None }), sink).unwrap();
        w.request(FetchWork::Span { identity: "broken".into(), from: ts("2026-01-05T00:00:00Z"), to: ts("2026-01-06T00:00:00Z") });
        match rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap() {
            FetchOutcome::Failed { identity, reason } => {
                assert_eq!(identity, "broken");
                assert_eq!(reason, "no such symbol");
            }
            other => panic!("{other:?}"),
        }
        w.shutdown();
    }

    #[test]
    fn identities_are_answered_and_shutdown_joins() {
        let (tx, rx) = channel();
        let sink: FetchOutcomeSink = Arc::new(move |o| { let _ = tx.send(o); });
        let mut w = FetchWorker::spawn("demo_kdb", Box::new(FakeFetch { calls: Default::default(), n: 1, catalogue: Some(vec!["VIX".into(), "SPX.close".into()]) }), sink).unwrap();
        w.request(FetchWork::Identities);
        match rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap() {
            FetchOutcome::Identities(Some(ids)) => assert_eq!(ids, vec!["VIX", "SPX.close"]),
            other => panic!("{other:?}"),
        }
        w.shutdown();
        assert!(!w.request(FetchWork::Identities), "a stopped worker refuses");
    }
}
```

`service.rs` tests (beside `subscribed_service_for`):

```rust
    /// A fetch adapter for the service tests: `FakeFetch` from
    /// `ingest::fetch`'s tests, wrapped as an `Adapter` named `fake_kdb`.
    struct FakeFetchAdapter { calls: Arc<std::sync::Mutex<Vec<crate::adapter::FetchRequest>>>, catalogue: Option<Vec<String>> }
    impl crate::adapter::Adapter for FakeFetchAdapter {
        fn name(&self) -> &'static str { "fake_kdb" }
        fn subscription(&self) -> Option<Box<dyn crate::adapter::Subscription>> { None }
        fn egress(&self) -> Option<Box<dyn crate::adapter::Egress>> { None }
        fn fetch(&self) -> Option<Box<dyn crate::adapter::Fetch>> {
            Some(Box::new(crate::ingest::fetch::tests::FakeFetch { calls: self.calls.clone(), n: 3, catalogue: self.catalogue.clone() }))
        }
    }

    fn fetch_service(
        catalogue: Option<Vec<String>>,
    ) -> (tempfile::TempDir, Arc<std::sync::Mutex<Vec<crate::adapter::FetchRequest>>>, DataService, std::sync::mpsc::Receiver<DataEvent>) {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut adapters = AdapterRegistry::default();
        adapters.register(Arc::new(FakeFetchAdapter { calls: calls.clone(), catalogue }));
        let mut schema = SchemaSpec::default();
        schema.datasets.push(crate::store::ddl::tests_support::series_dataset());
        let spec = crate::source::SourceSpec {
            adapter: "fake_kdb".to_string(),
            ..crate::source::SourceSpec::directory("kdb_hist", "series", Vec::new())
        };
        let (service, rx) = DataService::open_channel(DataServiceConfig {
            db_path: dir.path().join("geode.duckdb"),
            schema,
            views: Vec::new(),
            dimensions: DerivedDimensions::default(),
            query_workers: 2,
            sources: vec![spec],
            adapters,
            documents: Default::default(),
        })
        .unwrap();
        (dir, calls, service, rx)
    }

    fn next_series_fetched(rx: &std::sync::mpsc::Receiver<DataEvent>) -> (String, String, Result<u64, String>) {
        loop {
            match rx.recv_timeout(Duration::from_secs(60)).unwrap() {
                DataEvent::SeriesFetched { source, identity, result } => return (source, identity, result),
                _ => continue,
            }
        }
    }

    fn params(identity: &str, from: &str, to: &str) -> FetchParams {
        FetchParams { key: QueryKey(7), source: "kdb_hist".into(), identity: identity.into(), from: ts(from), to: ts(to) }
    }

    #[test]
    fn a_fetch_lands_rows_and_announces_the_pair() {
        let (_d, calls, service, rx) = fetch_service(None);
        service.fetch(&params("SPX.close", "2026-01-05T00:00:00Z", "2026-01-06T00:00:00Z"));
        let (source, identity, result) = next_series_fetched(&rx);
        assert_eq!((source.as_str(), identity.as_str()), ("kdb_hist", "SPX.close"));
        assert_eq!(result, Ok(2), "three bars, one NaN dropped");
        assert_eq!(calls.lock().unwrap().len(), 1);
        let n: i64 = service.conn.query_row("select count(*) from series_series", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn a_covered_span_is_answered_without_asking_the_source() {
        let (_d, calls, service, rx) = fetch_service(None);
        service.fetch(&params("SPX.close", "2026-01-05T00:00:00Z", "2026-01-06T00:00:00Z"));
        let _ = next_series_fetched(&rx);
        service.fetch(&params("SPX.close", "2026-01-05T00:00:00Z", "2026-01-06T00:00:00Z"));
        let (_, _, result) = next_series_fetched(&rx);
        assert_eq!(result, Ok(0));
        assert_eq!(calls.lock().unwrap().len(), 1, "no second call reached the adapter");
    }

    #[test]
    fn widening_the_range_fetches_only_the_gaps() {
        let (_d, calls, service, rx) = fetch_service(None);
        service.fetch(&params("SPX.close", "2026-01-05T00:00:00Z", "2026-01-06T00:00:00Z"));
        let _ = next_series_fetched(&rx);
        service.fetch(&params("SPX.close", "2026-01-04T00:00:00Z", "2026-01-07T00:00:00Z"));
        let _ = next_series_fetched(&rx);
        let _ = next_series_fetched(&rx);
        let calls = calls.lock().unwrap();
        let spans: Vec<(DateTime<Utc>, DateTime<Utc>)> = calls.iter().map(|c| (c.from, c.to)).collect();
        assert_eq!(spans, vec![
            (ts("2026-01-05T00:00:00Z"), ts("2026-01-06T00:00:00Z")),
            (ts("2026-01-04T00:00:00Z"), ts("2026-01-05T00:00:00Z")),
            (ts("2026-01-06T00:00:00Z"), ts("2026-01-07T00:00:00Z")),
        ]);
    }

    #[test]
    fn a_failed_fetch_is_a_load_lane_failure_keyed_by_the_pair_and_clears_on_success() {
        let (_d, _calls, service, rx) = fetch_service(None);
        service.fetch(&params("broken", "2026-01-05T00:00:00Z", "2026-01-06T00:00:00Z"));
        let (_, identity, result) = next_series_fetched(&rx);
        assert_eq!(identity, "broken");
        assert_eq!(result, Err("no such symbol".to_string()));
        let health = loop {
            match rx.recv_timeout(Duration::from_secs(60)).unwrap() {
                DataEvent::Health { source, worst, detail } => break (source, worst, detail),
                _ => continue,
            }
        };
        assert_eq!(health.0, "kdb_hist");
        assert!(matches!(health.1, Health::Failed { .. }));
        assert!(health.2.starts_with("broken@kdb_hist:"), "{}", health.2);
        drop(rx);
        // (The clear-on-success half is `HealthTracker`'s own contract, pinned by
        // `report_load_and_emit`'s tests; the seam here is that the SAME key is used
        // on both paths — see the `ingest_sink` arm below.)
        drop(service);
    }

    #[test]
    fn an_unknown_source_or_a_non_fetch_source_fails_at_once() {
        let (_d, _calls, service, rx) = fetch_service(None);
        service.fetch(&FetchParams { source: "nope".into(), ..params("SPX.close", "2026-01-05T00:00:00Z", "2026-01-06T00:00:00Z") });
        let (_, _, result) = next_series_fetched(&rx);
        assert_eq!(result, Err("source 'nope' is not a fetch source".to_string()));
    }

    #[test]
    fn a_fetch_source_whose_adapter_has_no_fetch_side_is_failed_on_the_discovery_lane_at_open() {
        let dir = tempfile::tempdir().unwrap();
        let (bus, _feed) = crate::adapter::ChannelAdapter::new("demo_bus");
        let mut adapters = AdapterRegistry::default();
        adapters.register(bus);
        let mut schema = SchemaSpec::default();
        schema.datasets.push(crate::store::ddl::tests_support::series_dataset());
        let spec = crate::source::SourceSpec { adapter: "demo_bus".into(), ..crate::source::SourceSpec::directory("kdb_hist", "series", Vec::new()) };
        let (_service, rx) = DataService::open_channel(DataServiceConfig {
            db_path: dir.path().join("geode.duckdb"), schema, views: Vec::new(),
            dimensions: DerivedDimensions::default(), query_workers: 2, sources: vec![spec], adapters, documents: Default::default(),
        }).unwrap();
        let health = loop {
            match rx.recv_timeout(Duration::from_secs(60)).unwrap() {
                DataEvent::Health { source, worst, detail } => break (source, worst, detail),
                _ => continue,
            }
        };
        assert_eq!(health.0, "kdb_hist");
        assert_eq!(health.1, Health::Failed { reason: "adapter 'demo_bus' has no fetch side".into() });
    }

    #[test]
    fn the_catalog_lists_series_spans_and_source_identities() {
        let (_d, _calls, service, rx) = fetch_service(Some(vec!["VIX".into(), "SPX.close".into()]));
        service.fetch(&params("SPX.close", "2026-01-05T00:00:00Z", "2026-01-06T00:00:00Z"));
        let _ = next_series_fetched(&rx);
        // Identities were requested at open; wait for them to land before reading.
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let snap = loop {
            let out = service.catalog(&CatalogParams { key: QueryKey(1), tag: 1, as_of: AsOf::Live });
            let snap = out.snapshot.unwrap();
            if !snap.identities.is_empty() || std::time::Instant::now() > deadline { break snap; }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(snap.identities, vec![("kdb_hist".to_string(), vec!["SPX.close".to_string(), "VIX".to_string()])], "sorted");
        let ds = snap.datasets.iter().find(|d| d.name == "series").unwrap();
        assert_eq!(ds.series.len(), 1);
        let s = &ds.series[0];
        assert_eq!((s.source.as_str(), s.identity.as_str(), s.fetches), ("kdb_hist", "SPX.close", 1));
        assert_eq!((s.from, s.to), (ts("2026-01-05T00:00:00Z"), ts("2026-01-06T00:00:00Z")));
    }
```

Make `FakeFetch` and `fetch::tests` `pub(crate)` (`#[cfg(test)] pub(crate) mod tests`) so the service tests can reuse it.

`handle.rs` test (beside the existing `for_tests` tests; imports `FetchParams`, `geode_core::query::QueryKey`, `chrono::Utc`):

```rust
    #[test]
    fn fetch_and_identities_are_queued_as_requests() {
        let (handle, rx) = DataHandle::for_tests();
        assert!(handle.fetch(FetchParams { key: QueryKey(3), source: "k".into(), identity: "SPX".into(), from: Utc::now(), to: Utc::now() }));
        assert!(handle.identities("k"));
        assert!(matches!(rx.recv().unwrap(), Request::Fetch(p) if p.identity == "SPX"));
        assert!(matches!(rx.recv().unwrap(), Request::Identities { source } if source == "k"));
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-data fetch`
Expected: compile errors (`FetchWorker`, `FetchParams`, `SeriesFetched` not found).

- [ ] **Step 3: Implement `ingest/fetch.rs`**

```rust
//! The fetch worker (timeseries spec §5.4): one thread per fetch source
//! that owns the adapter's `Fetch` and runs its blocking calls off every
//! other thread. It knows nothing about storage: an outcome goes to the
//! sink the service built, which submits rows to the ingest runner,
//! reports failures on the load lane, and stores identities.

use crate::adapter::{AdapterError, Fetch, FetchRequest, SeriesRows};
use crate::store::series::Span;
use chrono::{DateTime, Utc};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread::JoinHandle;

/// Queued requests per source before `request` refuses. A trader's chart
/// asks for a handful of gaps at a time; sixty-four is a burst.
pub const FETCH_BOUND: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchWork {
    Span {
        identity: String,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    },
    Identities,
}

#[derive(Debug)]
pub enum FetchOutcome {
    Fetched {
        identity: String,
        rows: SeriesRows,
        span: Span,
        /// Non-finite values dropped before the rows were handed on.
        dropped: usize,
    },
    Failed {
        identity: String,
        reason: String,
    },
    Identities(Option<Vec<String>>),
}

pub type FetchOutcomeSink = Arc<dyn Fn(FetchOutcome) + Send + Sync>;

pub struct FetchWorker {
    source: String,
    tx: Option<SyncSender<FetchWork>>,
    thread: Option<JoinHandle<()>>,
}

impl FetchWorker {
    pub fn spawn(
        source: &str,
        fetch: Box<dyn Fetch>,
        sink: FetchOutcomeSink,
    ) -> Result<FetchWorker, AdapterError> {
        let (tx, rx) = sync_channel(FETCH_BOUND);
        let name = format!("geode-fetch-{source}");
        let thread = std::thread::Builder::new()
            .name(name.clone())
            .spawn(move || run(fetch, rx, sink))
            .map_err(|e| AdapterError {
                message: format!("spawning {name}: {e}"),
            })?;
        Ok(FetchWorker {
            source: source.to_string(),
            tx: Some(tx),
            thread: Some(thread),
        })
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    /// Queue one unit of work. `false` means it was not queued — the
    /// bounded queue is full or the worker is gone — and the caller
    /// reports that as the request's outcome. Never blocks.
    pub fn request(&self, work: FetchWork) -> bool {
        match &self.tx {
            Some(tx) => !matches!(
                tx.try_send(work),
                Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_))
            ),
            None => false,
        }
    }

    /// Drop the sender (so `run`'s `recv` ends once the queue drains) and
    /// join. Idempotent; also on `Drop`.
    pub fn shutdown(&mut self) {
        self.tx.take();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for FetchWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run(mut fetch: Box<dyn Fetch>, rx: Receiver<FetchWork>, sink: FetchOutcomeSink) {
    while let Ok(work) = rx.recv() {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            geode_core::panic::contained(|| match work {
                FetchWork::Identities => FetchOutcome::Identities(fetch.catalogue()),
                FetchWork::Span { identity, from, to } => {
                    let req = FetchRequest {
                        identity: identity.clone(),
                        from,
                        to,
                    };
                    match fetch.fetch(&req).and_then(|mut rows| {
                        rows.validate()?;
                        let dropped = rows.drop_non_finite();
                        Ok((rows, dropped))
                    }) {
                        Ok((rows, dropped)) => {
                            if dropped > 0 {
                                tracing::warn!(
                                    target: "geode::ingest",
                                    "fetch of {identity}: {dropped} non-finite value(s) dropped"
                                );
                            }
                            FetchOutcome::Fetched {
                                identity,
                                rows,
                                span: (from, to),
                                dropped,
                            }
                        }
                        Err(e) => FetchOutcome::Failed {
                            identity,
                            reason: e.message,
                        },
                    }
                }
            })
        }));
        match outcome {
            Ok(o) => sink(o),
            Err(payload) => {
                let message = crate::ingest::runner::panic_payload_message(payload.as_ref());
                tracing::error!(target: "geode::ingest", "a fetch panicked: {message}");
            }
        }
    }
}
```

(`panic_payload_message` in `runner.rs` becomes `pub(crate)`.) Add `pub mod fetch;` to `ingest/mod.rs`.

- [ ] **Step 4: Implement the core additions**

`geode-core/src/query.rs`: add `SeriesCatalog` (derive `Clone, Debug, PartialEq, Eq`), the `series` field on `DatasetCatalog` (doc: "Series family only: one row per `(identity, source)` pair, from the coverage table (timeseries spec §4.6), never a data-table scan"), and `identities` on `CatalogSnapshot` (doc: "Each fetch source that answered a catalogue, with its identities sorted, for the picker's typeahead (spec §5.5)"). Both `Default`-derived structs keep deriving. Fix every literal the compiler names with `series: Vec::new(),` / `identities: Vec::new(),`.

`query/catalog.rs::dataset_catalog`: before the `Ok(DatasetCatalog { … })`, add

```rust
    let series = if ds.is_series() {
        crate::store::series::series_catalog(conn, &ds.name)?
    } else {
        Vec::new()
    };
```

and put `series,` in the literal. `live_rows` for a series dataset: `sizes.get(&crate::store::series::series_table(&ds.name))` (add an `if ds.is_series()` branch before the `table_pairs` loop).

`store/series.rs`: add

```rust
/// One row per pair from the coverage table (timeseries spec §4.6):
/// the hull of its fetched spans, how many fetches, and the newest
/// `received_at`. Coverage is one row per fetch, so this is
/// catalog-sized and never scans the series table — the rule
/// `query::catalog::build_catalog` states.
pub fn series_catalog(conn: &Connection, dataset: &str) -> Result<Vec<geode_core::query::SeriesCatalog>, StoreError> {
    let sql = format!(
        "select source, series_id, epoch_us(min(from_ts)), epoch_us(max(to_ts)), count(*), \
         epoch_us(max(received_at)) from {} group by source, series_id order by series_id, source",
        coverage_table(dataset)
    );
    let mut stmt = conn.prepare(&sql).map_err(sql_err(&sql))?;
    let rows = stmt
        .query_map([], |r| {
            Ok(geode_core::query::SeriesCatalog {
                source: r.get(0)?,
                identity: r.get(1)?,
                from: from_micros(r.get::<_, i64>(2)?),
                to: from_micros(r.get::<_, i64>(3)?),
                fetches: r.get::<_, i64>(4)? as u64,
                latest_received_at: from_micros(r.get::<_, i64>(5)?),
            })
        })
        .map_err(sql_err(&sql))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(sql_err(&sql))?);
    }
    Ok(out)
}
```

- [ ] **Step 5: Implement the handle and service additions**

`handle.rs`: add `Fetch(FetchParams)` and `Identities { source: String }` to `Request` (doc: "The timeseries viewer's on-demand fetch (timeseries spec §5.3): coverage is subtracted on the service thread and only the gaps reach the source's fetch worker; the outcome is `DataEvent::SeriesFetched`, keyed by the pair, never by `key`" / "Ask a fetch source for its identities again; they land in the next `CatalogSnapshot::identities`"), the two `DataHandle` methods, and two `serve` arms:

```rust
            Request::Fetch(params) => service.fetch(&params),
            Request::Identities { source } => {
                if !service.identities(&source) {
                    tracing::warn!(target: "geode::ingest", "identities request for '{source}' refused");
                }
            }
```

`service.rs`:

1. `DataEvent::SeriesFetched { source: String, identity: String, result: Result<u64, String> }` after `Published`, doc: "A fetch finished (timeseries spec §5.4), keyed by the PAIR rather than the asking tile: two tiles holding `SPX.close@kdb_hist` both learn the outcome of the one fetch that answered them. `Ok(appended)` may be `Ok(0)` — a covered span, or an overlapping refetch — and the tile must requery on it all the same, since coverage changed."

2. `FetchParams` (derive `Debug, Clone, PartialEq, Eq`), re-exported from `lib.rs`.

3. `DataService` gains, after `subscriptions` (drop order: a fetch worker's sink submits into `ingest`, so the workers must stop before the runner): `fetchers: std::sync::Mutex<Vec<FetchWorker>>`, and anywhere: `identities: Arc<std::sync::Mutex<std::collections::BTreeMap<String, Vec<String>>>>`, `fetch_datasets: std::collections::HashMap<String, String>` (source name → dataset name, for `fetch`).

4. In `open`, the resolution loop becomes a `match spec.shape(&config.schema)`:

```rust
        let mut fetchers: Vec<FetchWorker> = Vec::new();
        let mut fetch_datasets = std::collections::HashMap::new();
        let identities: Arc<std::sync::Mutex<std::collections::BTreeMap<String, Vec<String>>>> = Default::default();
        for spec in &config.sources {
            match spec.shape(&config.schema) {
                SourceShape::Directory => {
                    directory_sources.push(spec.clone());
                    continue;
                }
                SourceShape::Subscribed => {}
                SourceShape::Fetch => {
                    let report_unservable = …the same closure as below, verbatim…;
                    let Some(adapter) = config.adapters.get(&spec.adapter) else {
                        report_unservable(format!("adapter '{}' is not in this build", spec.adapter));
                        continue;
                    };
                    let Some(fetch) = adapter.fetch() else {
                        report_unservable(format!("adapter '{}' has no fetch side", spec.adapter));
                        continue;
                    };
                    let report_load: LoadReportSink = …the subscribed arm's `report_load`, verbatim…;
                    let outcome_sink: FetchOutcomeSink = {
                        let ingest = Arc::clone(&ingest);
                        let sink = Arc::clone(&sink);
                        let identities = Arc::clone(&identities);
                        let source = spec.name.clone();
                        let dataset = spec.dataset.clone();
                        Arc::new(move |outcome| match outcome {
                            FetchOutcome::Fetched { identity, rows, span, .. } => {
                                ingest.submit_series(SeriesJob {
                                    source: source.clone(),
                                    dataset: dataset.clone(),
                                    identity,
                                    rows,
                                    span,
                                    received_at: Utc::now(),
                                });
                            }
                            FetchOutcome::Failed { identity, reason } => {
                                let pair = format!("{identity}@{source}");
                                report_load(&pair, Health::Failed { reason: reason.clone() }, format!("{pair}: {reason}"));
                                let _ = sink(DataEvent::SeriesFetched {
                                    source: source.clone(),
                                    identity,
                                    result: Err(reason),
                                });
                            }
                            FetchOutcome::Identities(Some(mut ids)) => {
                                ids.sort();
                                ids.dedup();
                                identities.lock().unwrap_or_else(|e| e.into_inner()).insert(source.clone(), ids);
                            }
                            FetchOutcome::Identities(None) => {}
                        })
                    };
                    match FetchWorker::spawn(&spec.name, fetch, outcome_sink) {
                        Ok(worker) => {
                            // Servable: the discovery lane's clean state, so a
                            // later failure reads as a transition.
                            health_tracker.report_discovery_and_emit(&spec.name, Health::Ok, String::new(), |_| true);
                            worker.request(FetchWork::Identities);
                            fetch_datasets.insert(spec.name.clone(), spec.dataset.clone());
                            fetchers.push(worker);
                        }
                        Err(e) => report_unservable(e.message),
                    }
                    continue;
                }
            }
            …the existing subscribed body, unchanged…
```

Hoist `report_unservable` above the `match` so both arms share it (it only borrows `spec`, `sink`, `health_tracker`).

5. The `ingest_sink` arms from Task 7 become:

```rust
                IngestEvent::SeriesAppended { source, dataset, identity, appended, swept } => {
                    tracing::info!(
                        target: "geode::ingest",
                        "appended {identity}@{source} into {dataset}: {appended} row(s), {swept} swept",
                    );
                    let pair = format!("{identity}@{source}");
                    let health_delivered = health_tracker.report_load_and_emit(
                        &source, &pair, Health::Ok, String::new(),
                        |reported| match reported {
                            Some((worst, detail)) => {
                                log_health_event(&source, &worst, &detail);
                                sink(DataEvent::Health { source: source.clone(), worst, detail })
                            }
                            None => true,
                        },
                    );
                    let delivered = sink(DataEvent::SeriesFetched {
                        source: source.clone(),
                        identity,
                        result: Ok(appended as u64),
                    });
                    let _ = sink(DataEvent::LoadEnded);
                    delivered && health_delivered
                }
                IngestEvent::SeriesFailed { source, dataset, identity, reason } => {
                    let pair = format!("{identity}@{source}");
                    log_ingest_failure(&dataset, &pair, &reason);
                    let health_delivered = health_tracker.report_load_and_emit(
                        &source, &pair, Health::Failed { reason: reason.clone() }, format!("{pair}: {reason}"),
                        |reported| match reported {
                            Some((worst, detail)) => sink(DataEvent::Health { source: source.clone(), worst, detail }),
                            None => true,
                        },
                    );
                    let delivered = sink(DataEvent::SeriesFetched {
                        source: source.clone(),
                        identity,
                        result: Err(reason),
                    });
                    let _ = sink(DataEvent::LoadEnded);
                    delivered && health_delivered
                }
```

The load-lane key is `"{identity}@{source}"` on every path (the worker's `Failed`, the runner's `SeriesFailed`, the runner's `SeriesAppended`), which is what lets a success clear a failure.

6. `DataService::fetch`:

```rust
    /// The on-demand fetch (timeseries spec §5.4): subtract what the
    /// coverage table already holds and queue one job per gap on the
    /// source's fetch worker. Every early exit is a `SeriesFetched`, so
    /// the asking tile always hears back.
    pub fn fetch(&self, params: &FetchParams) {
        let answer = |result: Result<u64, String>| {
            let _ = (self.sink)(DataEvent::SeriesFetched {
                source: params.source.clone(),
                identity: params.identity.clone(),
                result,
            });
        };
        let Some(dataset) = self.fetch_datasets.get(&params.source) else {
            answer(Err(format!("source '{}' is not a fetch source", params.source)));
            return;
        };
        let loaded = match crate::store::series::coverage(&self.conn, dataset, &params.source, &params.identity) {
            Ok(spans) => spans,
            Err(e) => {
                answer(Err(format!("reading coverage: {e}")));
                return;
            }
        };
        let gaps = crate::store::series::missing_spans((params.from, params.to), &loaded);
        if gaps.is_empty() {
            answer(Ok(0));
            return;
        }
        let fetchers = self.fetchers.lock().unwrap_or_else(|e| e.into_inner());
        let Some(worker) = fetchers.iter().find(|w| w.source() == params.source) else {
            answer(Err(format!("source '{}' is not a fetch source", params.source)));
            return;
        };
        for (from, to) in gaps {
            if !worker.request(FetchWork::Span { identity: params.identity.clone(), from, to }) {
                answer(Err(format!("the fetch queue for '{}' is full", params.source)));
                return;
            }
        }
    }

    pub fn identities(&self, source: &str) -> bool {
        let fetchers = self.fetchers.lock().unwrap_or_else(|e| e.into_inner());
        fetchers.iter().find(|w| w.source() == source).is_some_and(|w| w.request(FetchWork::Identities))
    }
```

This needs `DataService` to hold `sink: EventSink` (clone the `Arc` into the struct in `open`, as `conn` is kept) — add the field if `open` does not already keep one.

7. `DataService::catalog`: after `build_catalog`, `if let Ok(snap) = &mut snapshot { snap.identities = self.identities.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|(s, ids)| (s.clone(), ids.clone())).collect(); }`.

8. `DataService::shutdown`: stop the fetch workers first (`for w in self.fetchers.lock()….iter_mut() { w.shutdown(); }`), before the subscriptions.

- [ ] **Step 6: Run the tests and the workspace build**

Run: `cargo test -p geode-data && cargo check --workspace --all-targets && cargo check -p geode-shell --features test-support --all-targets`
Expected: every test passes; the compiler names each `DatasetCatalog`/`CatalogSnapshot` literal outside `geode-data` until it has the new field; `bridge.rs`'s `match event` fails on the new variant — add, inside the `window.update` match, before `DataEvent::LoadEnded`:

```rust
                    // Timeseries spec §5.4: routed to the timeseries tiles in
                    // Part 2 (`Delivery::SeriesFetched`). Until then the data
                    // crate's own log line at `info` is the record; nothing
                    // here logs, per the UI-thread level constraint.
                    DataEvent::SeriesFetched { .. } => {}
```

- [ ] **Step 7: Commit**

```bash
git add crates/geode-core/src/query.rs crates/geode-data crates/geode-app/src/bridge.rs crates/geode-shell crates/geode-marketdata crates/geode-diagnostics
git commit -m "data: Request::Fetch, the fetch worker, DataEvent::SeriesFetched, series catalog rows and identities"
```

---
### Task 9: The demo adapter and the `--demo` wiring

**Files:**
- Create: `crates/geode-app/src/demo_series.rs`
- Modify: `crates/geode-app/src/main.rs` (`mod demo_bus;` at 8; the adapter block at 111–135), `crates/geode-app/src/demo.rs` (`layer` at 38–82 and its tests), `examples/demo-config/datasets.toml` (append)
- Test: `crates/geode-app/src/demo_series.rs` (`mod tests`), `crates/geode-app/src/demo.rs` (`mod tests`)

**Interfaces:**
- Produces: `geode_app::demo_series::DemoSeries` (`pub fn new(name: &'static str, seed: u64, catalogue: bool) -> Arc<DemoSeries>`, `impl Adapter` with `fetch()` answering a fresh `DemoFetch`), `pub const IDENTITIES: [(&str, f64, f64, f64); 24]` (name, level, drift per day, daily vol), `pub fn bars(seed: u64, identity: &str, from: DateTime<Utc>, to: DateTime<Utc>) -> Option<SeriesRows>` (`None` for an unknown identity).
- Two demo sources, `demo_kdb` (catalogue) and `demo_rest` (none), over the demo `[series]` dataset (`retention = "7d"`).

- [ ] **Step 1: Write the failing tests**

`demo_series.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use geode_data::adapter::{Adapter, Fetch, FetchRequest};

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn the_same_span_always_yields_the_same_bars() {
        let a = bars(42, "SPX.close", t("2026-01-05T00:00:00Z"), t("2026-01-07T00:00:00Z")).unwrap();
        let b = bars(42, "SPX.close", t("2026-01-05T00:00:00Z"), t("2026-01-07T00:00:00Z")).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), 2 * 390, "two weekdays of one-minute bars, 14:30–21:00 UTC");
        assert!(a.validate().is_ok());
        assert!(a.ts.first().unwrap() >= &t("2026-01-05T00:00:00Z"));
        assert!(a.ts.last().unwrap() < &t("2026-01-07T00:00:00Z"));
    }

    #[test]
    fn overlapping_spans_agree_on_their_shared_bars() {
        let wide = bars(42, "SPX.close", t("2026-01-05T00:00:00Z"), t("2026-01-08T00:00:00Z")).unwrap();
        let narrow = bars(42, "SPX.close", t("2026-01-06T00:00:00Z"), t("2026-01-07T00:00:00Z")).unwrap();
        let shared: Vec<(DateTime<Utc>, f64)> = wide.ts.iter().copied().zip(wide.value.iter().copied())
            .filter(|(ts, _)| *ts >= t("2026-01-06T00:00:00Z") && *ts < t("2026-01-07T00:00:00Z")).collect();
        let narrow_pairs: Vec<(DateTime<Utc>, f64)> = narrow.ts.iter().copied().zip(narrow.value.iter().copied()).collect();
        assert_eq!(shared, narrow_pairs, "a span-independent generator: what the coverage subtraction relies on");
    }

    #[test]
    fn weekends_have_no_bars_and_a_different_seed_differs() {
        let sat = bars(42, "SPX.close", t("2026-01-10T00:00:00Z"), t("2026-01-12T00:00:00Z")).unwrap();
        assert!(sat.is_empty(), "Saturday and Sunday");
        let a = bars(42, "VIX", t("2026-01-05T00:00:00Z"), t("2026-01-06T00:00:00Z")).unwrap();
        let b = bars(43, "VIX", t("2026-01-05T00:00:00Z"), t("2026-01-06T00:00:00Z")).unwrap();
        assert_ne!(a.value, b.value);
        assert!(a.value.iter().all(|v| *v > 0.0), "a geometric walk stays positive");
    }

    #[test]
    fn the_two_demo_sources_differ_only_in_the_catalogue() {
        let kdb = DemoSeries::new("demo_kdb", 42, true);
        let rest = DemoSeries::new("demo_rest", 42, false);
        assert_eq!(kdb.name(), "demo_kdb");
        assert!(kdb.subscription().is_none() && kdb.egress().is_none());
        let mut k = kdb.fetch().unwrap();
        let mut r = rest.fetch().unwrap();
        let ids = k.catalogue().unwrap();
        assert_eq!(ids.len(), IDENTITIES.len());
        assert!(ids.contains(&"SPX.close".to_string()) && ids.contains(&"VIX".to_string()));
        assert!(r.catalogue().is_none());
        let req = FetchRequest { identity: "SX5E.close".into(), from: t("2026-01-05T00:00:00Z"), to: t("2026-01-06T00:00:00Z") };
        assert_eq!(k.fetch(&req).unwrap(), r.fetch(&req).unwrap());
        let unknown = FetchRequest { identity: "NOPE".into(), ..req };
        assert_eq!(k.fetch(&unknown).unwrap_err().message, "unknown identity 'NOPE'");
    }
}
```

`demo.rs` tests — extend `the_demo_layer_declares_the_cvi_source` (or add a sibling) so that after `Config::load(…)` the sources doc yields, through `SourceSpec::from_doc` against the demo schema, a `demo_kdb` and a `demo_rest` source whose `shape(&schema)` is `SourceShape::Fetch`, with no diagnostics; and assert `sources.table["demo_kdb"]["dataset"].as_str() == Some("series")`.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-app demo_series`
Expected: compile error, module not found.

- [ ] **Step 3: Implement `demo_series.rs`**

```rust
//! The demo's fetch adapter (timeseries spec §5.6): a seeded,
//! deterministic, span-independent generator of one-minute bars on
//! weekdays 14:30–21:00 UTC (New York's session, without a calendar) for
//! two dozen identities. Two sources share it under `--demo`: `demo_kdb`
//! offers a catalogue, `demo_rest` does not, so both picker paths are
//! exercised.
//!
//! Span-independence is the property that matters: a request for
//! `[a, b)` returns exactly the bars a wider request would return inside
//! `[a, b)`, so an overlapping refetch appends nothing (spec §4.4 step 2)
//! and the service's coverage subtraction is honest. It comes from
//! seeding per `(seed, identity, day)` and walking each day from a daily
//! level that is itself walked from a fixed epoch — never from "the last
//! bar this process generated".

use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc, Weekday};
use geode_data::adapter::{Adapter, AdapterError, Egress, Fetch, FetchRequest, SeriesRows, Subscription};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::sync::Arc;

/// `(identity, level, drift per day, daily vol)`.
pub const IDENTITIES: [(&str, f64, f64, f64); 24] = [
    ("SPX.close", 5600.0, 0.0003, 0.010),
    ("SPX.vol_1m", 14.0, 0.0, 0.060),
    ("SPX.vol_3m", 15.5, 0.0, 0.045),
    ("SPX.skew_3m", 1.8, 0.0, 0.030),
    ("SX5E.close", 5100.0, 0.0002, 0.011),
    ("SX5E.vol_1m", 15.0, 0.0, 0.060),
    ("SX5E.vol_3m", 16.0, 0.0, 0.045),
    ("NKY.close", 39000.0, 0.0003, 0.013),
    ("NKY.vol_1m", 18.0, 0.0, 0.065),
    ("NDX.close", 20000.0, 0.0004, 0.013),
    ("NDX.vol_1m", 18.5, 0.0, 0.060),
    ("RTY.close", 2200.0, 0.0002, 0.014),
    ("RTY.vol_1m", 20.0, 0.0, 0.060),
    ("VIX", 16.0, 0.0, 0.070),
    ("V2X", 17.0, 0.0, 0.070),
    ("VNKY", 19.0, 0.0, 0.070),
    ("SPX.fwd_1y", 5720.0, 0.0003, 0.010),
    ("SX5E.fwd_1y", 5150.0, 0.0002, 0.011),
    ("SPX.div_1y", 1.4, 0.0, 0.004),
    ("SX5E.div_1y", 3.2, 0.0, 0.004),
    ("SPX.repo_1y", 0.35, 0.0, 0.020),
    ("SX5E.repo_1y", 0.55, 0.0, 0.020),
    ("EURUSD", 1.09, 0.0, 0.005),
    ("USDJPY", 152.0, 0.0, 0.006),
];

/// A Monday, and the day the daily walk starts from.
const EPOCH: NaiveDate = match NaiveDate::from_ymd_opt(2020, 1, 6) {
    Some(d) => d,
    None => unreachable!(),
};
const OPEN_MINUTE: i64 = 14 * 60 + 30;
const BARS_PER_DAY: i64 = 390;

fn fnv(seed: u64, identity: &str, day: i64) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325 ^ seed;
    for b in identity.bytes().chain(day.to_le_bytes()) {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// An approximately normal step: Irwin–Hall over twelve uniforms, mean 0,
/// unit variance. Enough for a demo walk; `rand_distr` is not a dep.
fn step(rng: &mut StdRng) -> f64 {
    (0..12).map(|_| rng.random::<f64>()).sum::<f64>() - 6.0
}

fn is_weekday(d: NaiveDate) -> bool {
    !matches!(d.weekday(), Weekday::Sat | Weekday::Sun)
}

/// The level at the open of `day`, walked from `EPOCH` one weekday at a
/// time with a per-(seed, identity) rng, so it depends on nothing but
/// the calendar day.
fn open_level(seed: u64, identity: &str, level: f64, drift: f64, vol: f64, day: NaiveDate) -> f64 {
    let mut rng = StdRng::seed_from_u64(fnv(seed, identity, -1));
    let mut x = level.ln();
    let mut d = EPOCH;
    while d < day {
        if is_weekday(d) {
            x += drift + vol * step(&mut rng);
        }
        d += Duration::days(1);
    }
    x.exp()
}

pub fn bars(seed: u64, identity: &str, from: DateTime<Utc>, to: DateTime<Utc>) -> Option<SeriesRows> {
    let &(_, level, drift, vol) = IDENTITIES.iter().find(|(n, ..)| *n == identity)?;
    let mut rows = SeriesRows::default();
    let intraday_vol = vol / (BARS_PER_DAY as f64).sqrt();
    let mut day = from.date_naive();
    let last = to.date_naive();
    while day <= last {
        if is_weekday(day) && day >= EPOCH {
            let mut rng = StdRng::seed_from_u64(fnv(seed, identity, day.num_days_from_ce() as i64));
            let mut x = open_level(seed, identity, level, drift, vol, day).ln();
            for k in 0..BARS_PER_DAY {
                x += intraday_vol * step(&mut rng);
                let ts = Utc.from_utc_datetime(&day.and_hms_opt(0, 0, 0).expect("midnight")) + Duration::minutes(OPEN_MINUTE + k);
                if ts >= from && ts < to {
                    rows.ts.push(ts);
                    rows.value.push(x.exp());
                }
            }
        }
        day += Duration::days(1);
    }
    Some(rows)
}

pub struct DemoSeries {
    name: &'static str,
    seed: u64,
    catalogue: bool,
}

impl DemoSeries {
    pub fn new(name: &'static str, seed: u64, catalogue: bool) -> Arc<DemoSeries> {
        Arc::new(DemoSeries { name, seed, catalogue })
    }
}

struct DemoFetch {
    seed: u64,
    catalogue: bool,
}

impl Fetch for DemoFetch {
    fn fetch(&mut self, req: &FetchRequest) -> Result<SeriesRows, AdapterError> {
        bars(self.seed, &req.identity, req.from, req.to).ok_or_else(|| AdapterError {
            message: format!("unknown identity '{}'", req.identity),
        })
    }

    fn catalogue(&mut self) -> Option<Vec<String>> {
        self.catalogue
            .then(|| IDENTITIES.iter().map(|(n, ..)| n.to_string()).collect())
    }
}

impl Adapter for DemoSeries {
    fn name(&self) -> &'static str {
        self.name
    }
    fn subscription(&self) -> Option<Box<dyn Subscription>> {
        None
    }
    fn egress(&self) -> Option<Box<dyn Egress>> {
        None
    }
    fn fetch(&self) -> Option<Box<dyn Fetch>> {
        Some(Box::new(DemoFetch { seed: self.seed, catalogue: self.catalogue }))
    }
}
```

`open_level` walks from `EPOCH` per call: ~1,600 weekdays of twelve `random::<f64>()` each, well under a millisecond, and it runs once per day generated. If a profile ever shows it, memoise per `(identity, day)` inside `DemoFetch`; do not change the seeding.

- [ ] **Step 4: Wire the demo**

`main.rs`: add `mod demo_series;` beside `mod demo_bus;`, and inside the `if demo_rows.is_some()` block, after `adapters.register(adapter);`:

```rust
                // The timeseries demo sources (timeseries spec §5.6): the
                // same seed as the risk generator, one with a catalogue
                // and one without.
                adapters.register(demo_series::DemoSeries::new("demo_kdb", 42, true));
                adapters.register(demo_series::DemoSeries::new("demo_rest", 42, false));
```

`demo.rs::layer`: append to the `sources` format string, after the `[cvi]` table:

```
[demo_kdb]\nadapter = \"demo_kdb\"\ndataset = \"series\"\n\
[demo_rest]\nadapter = \"demo_rest\"\ndataset = \"series\"\n
```

and update the function's doc comment to name the two fetch sources.

`examples/demo-config/datasets.toml`: append

```toml

# The timeseries viewer's cache (timeseries spec §4.2): every fetch source
# in `sources` fills this one table. The family implies its columns.
[series]
family = "series"
retention = "7d"
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p geode-app && cargo test -p geode-data --bench publish_document --no-run`
Expected: PASS (the bench reads the same `datasets.toml`; `SchemaSpec::from_doc` tolerates the new stanza).

Then run the app once and confirm the log shows both demo sources opening clean:

Run: `cargo run -p geode-app -- --demo 1000 2>&1 | grep -i "demo_kdb\|demo_rest\|series" | head` (quit with `mod+q` or ctrl-c after the window opens)
Expected: no `Failed` line for either source; a `geode::ingest` line is not expected until Part 4 sends a fetch.

- [ ] **Step 6: Commit**

```bash
git add crates/geode-app examples/demo-config/datasets.toml
git commit -m "app: the demo_series fetch adapter and two demo fetch sources"
```

---

### Task 10: Harness entries, bench, docs, spec as-built

**Files:**
- Modify: `scripts/mutation-check.sh` (append entries; update the count in `CLAUDE.md:126`)
- Create: `crates/geode-data/benches/append_series.rs`; modify `crates/geode-data/Cargo.toml` (`[[bench]]`), `docs/perf.md`
- Modify: `CLAUDE.md` (status row, load-bearing rules), `docs/phase-history.md`, the spec (§4.10 "As built (Part 1)")

- [ ] **Step 1: Harness entries** (append before the anchors-only verification block at the end of the script; each anchor is a verbatim substring of the code as written above — re-read the file before anchoring, and run `--anchors-only` after)

```sh
run_mutation "series: an unchanged row is appended again" \
  crates/geode-data/src/store/series.rs \
  'where live.ts = make_timestamp(s.ts_us) and live.v = s.value' \
  'where false' \
  geode-data an_overlapping_refetch_with_the_same_values_appends_nothing_but_records_coverage

run_mutation "series: dedupe compares against any version, not the live one" \
  crates/geode-data/src/store/series.rs \
  'select ts, arg_max(value, received_at) as v from {table}' \
  'select ts, arg_min(value, received_at) as v from {table}' \
  geode-data a_corrected_value_is_one_more_row_and_the_older_one_survives

run_mutation "series: coverage is not recorded for an empty fetch" \
  crates/geode-data/src/store/series.rs \
  '    // 4. Coverage, always.' \
  '    // 4. Coverage, always.
    if req.rows.is_empty() { return Ok(SeriesAppended { appended, swept: 0 }); }' \
  geode-data an_empty_fetch_records_coverage_and_appends_nothing

run_mutation "series: retention deletes the live row too" \
  crates/geode-data/src/store/series.rs \
  'and n.series_id = t.series_id and n.ts = t.ts and n.received_at > t.received_at)' \
  'and n.series_id = t.series_id and n.ts = t.ts)' \
  geode-data retention_deletes_superseded_rows_older_than_the_window_and_keeps_live

run_mutation "series: history keeps the coverage rows it should drop" \
  crates/geode-data/src/store/series.rs \
  '"delete from {} where source = ? and series_id = ? and epoch_us(to_ts) <= ?",' \
  '"delete from {} where source = ? and series_id = ? and false",' \
  geode-data history_deletes_rows_and_coverage_whose_ts_is_too_old

run_mutation "series: missing_spans ignores loaded spans" \
  crates/geode-data/src/store/series.rs \
  '    let mut loaded = loaded.to_vec();
    merge_spans(&mut loaded);' \
  '    let loaded: Vec<Span> = Vec::new();' \
  geode-data missing_spans_subtracts_loaded_spans

run_mutation "service: a covered span still reaches the source" \
  crates/geode-data/src/service.rs \
  '        if gaps.is_empty() {
            answer(Ok(0));
            return;
        }' \
  '        let gaps = vec![(params.from, params.to)];' \
  geode-data a_covered_span_is_answered_without_asking_the_source

run_mutation "service: the fetch failure lane key differs from the success key" \
  crates/geode-data/src/service.rs \
  '                                report_load(&pair, Health::Failed { reason: reason.clone() }, format!("{pair}: {reason}"));' \
  '                                report_load(&identity, Health::Failed { reason: reason.clone() }, format!("{pair}: {reason}"));' \
  geode-data a_failed_fetch_is_a_load_lane_failure_keyed_by_the_pair_and_clears_on_success

run_mutation "runner: series jobs are taken after files" \
  crates/geode-data/src/ingest/runner.rs \
  '    if let Some(job) = q.series.pop_front() {
        return Some(Work::Series(job));
    }
    if q.items.is_empty() {
        return None;
    }' \
  '    if q.items.is_empty() {
        if let Some(job) = q.series.pop_front() {
            return Some(Work::Series(job));
        }
        return None;
    }' \
  geode-data take_work_pops_documents_then_series_then_files

run_mutation "fetch: non-finite values are handed on" \
  crates/geode-data/src/ingest/fetch.rs \
  '                        let dropped = rows.drop_non_finite();' \
  '                        let dropped = 0;' \
  geode-data a_span_request_yields_fetched_rows_with_non_finite_values_dropped

run_mutation "core: a series dataset keeps a declared column" \
  crates/geode-core/src/schema/mod.rs \
  '    ds.columns.clear();
    for (field, list) in [("key", &mut ds.key), ("axes", &mut ds.axes)] {' \
  '    for (field, list) in [("key", &mut ds.key), ("axes", &mut ds.axes)] {' \
  geode-core a_declared_column_on_a_series_dataset_is_an_error_and_dropped

run_mutation "core: a fetch source is classified as subscribed" \
  crates/geode-core/src/source_config.rs \
  '        } else if schema.dataset(&self.dataset).is_some_and(|d| d.is_series()) {
            SourceShape::Fetch' \
  '        } else if false {
            SourceShape::Fetch' \
  geode-core shape_names_all_three
```

The "fetch failure lane key" entry's test must observe the clear-on-success half for the mutation to be caught: extend `a_failed_fetch_is_a_load_lane_failure_keyed_by_the_pair_and_clears_on_success` so that after the failure it fetches a good identity, then asserts the next `DataEvent::Health` for `kdb_hist` is NOT `Ok` (the `broken` batch is still failed under the pair key, and the good pair's `Ok` does not clear it) — and then, with a second `FakeFetch` mode that succeeds for `broken` on its second call (add a `fail_once: bool` to `FakeFetch`), that a retry of `broken` clears the lane to `Ok`. Under the mutation, the failure is keyed `broken` and the success `broken@kdb_hist`, so the clear never happens and the test fails.

Run: `zsh scripts/mutation-check.sh --anchors-only && zsh scripts/mutation-check.sh "series:" && zsh scripts/mutation-check.sh "fetch:" && zsh scripts/mutation-check.sh "runner: series"`
Expected: no ANCHOR/AMBIG; every entry `caught` by its named test (no `caught*`, no `SURVIVED`). Update `CLAUDE.md:126`'s entry count (`1055` → the new `grep -c '^run_mutation' scripts/mutation-check.sh`).

- [ ] **Step 2: The append bench**

`crates/geode-data/benches/append_series.rs`:

```rust
//! `append_series`'s cost (timeseries spec §4.4) at two shapes: one day
//! of minute bars (390 rows, a widening by a day) and two years (196,560
//! rows, a first load). Each iteration gets its own temp store, the
//! `publish_document` bench's shape; the timed half appends into a table
//! that already holds the same span once, so what is measured is the
//! steady state — dedupe against live rows plus the insert — not the
//! first insert into an empty table.

use chrono::{DateTime, Duration, Utc};
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use geode_core::config::{LayerDoc, merge_docs};
use geode_core::schema::{DatasetSpec, SchemaSpec};
use geode_data::adapter::SeriesRows;
use geode_data::store::series::{SeriesAppendRequest, append_series};
use geode_data::store::{Catalog, Store};

fn series_dataset() -> DatasetSpec {
    let text = include_str!("../../../examples/demo-config/datasets.toml");
    let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
    SchemaSpec::from_doc(&doc).0.dataset("series").unwrap().clone()
}

fn rows(n: usize, first: f64) -> SeriesRows {
    let start: DateTime<Utc> = "2024-01-02T14:30:00Z".parse().unwrap();
    SeriesRows {
        ts: (0..n).map(|i| start + Duration::minutes(i as i64)).collect(),
        value: (0..n).map(|i| first + (i % 97) as f64 * 0.01).collect(),
    }
}

fn bench(c: &mut Criterion) {
    let ds = series_dataset();
    for (label, n) in [("390", 390usize), ("196560", 196_560)] {
        c.bench_function(&format!("append_series/{label}"), |b| {
            b.iter_batched(
                || {
                    let dir = tempfile::tempdir().unwrap();
                    let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
                    store.apply_schema(&ds).unwrap();
                    Catalog::new(store.writer()).ensure_tables().unwrap();
                    let first = rows(n, 100.0);
                    let span = (first.ts[0], *first.ts.last().unwrap() + Duration::minutes(1));
                    append_series(&store, &SeriesAppendRequest { dataset: &ds, source: "b", identity: "SPX.close", rows: &first, span, received_at: Utc::now() }).unwrap();
                    (dir, store, rows(n, 100.5), span)
                },
                |(dir, store, second, span)| {
                    append_series(&store, &SeriesAppendRequest { dataset: &ds, source: "b", identity: "SPX.close", rows: &second, span, received_at: Utc::now() }).unwrap();
                    drop(store);
                    drop(dir);
                },
                BatchSize::PerIteration,
            )
        });
    }
}

criterion_group!(benches, bench);
criterion_main!(benches);
```

Add to `geode-data/Cargo.toml`: `[[bench]]\nname = "append_series"\nharness = false`. Run `cargo bench -p geode-data --bench append_series` and record both medians in `docs/perf.md` under a new `## Timeseries (spec §4.4, Part 1)` section in the existing table shape, with the conditions line. Note there that the 196,560-row figure is a first-load bound, not a steady-state one, and that the dedupe's `arg_max` is a full scan of the pair, which is what a later index or partitioning would target if it ever shows.

- [ ] **Step 3: Docs**

- `CLAUDE.md`: add a status-table row `Timeseries Part 1 (data tier) | series family (bitemporal append-only), Fetch adapter shape, Request::Fetch with coverage subtraction, per-pair retention, series catalog rows, demo_series | 2026-09-19-…timeseries-viewer`, and these load-bearing rules under **Data layer**:
  - "Series family: five implied columns in `SERIES_COLUMNS` order, never declared; live is `arg_max(value, received_at)` per `(source, series_id, ts)`; `append_series` is the one door and drops rows equal to LIVE before insert; coverage is written even for an empty fetch; retention runs per pair inside the append transaction (there is no production sweeper); both timestamps are naive UTC `TIMESTAMP` bound as micros."
  - "A fetch source is `SourceShape::Fetch` (non-directory adapter over a series dataset); `is_subscribed()` still means 'not a directory'. The load-lane key for a series is `"{identity}@{source}"` on every path, which is what lets a success clear a failure. `DataEvent::SeriesFetched` is keyed by the pair, never the asking tile, and `Ok(0)` still means requery."
- `docs/phase-history.md`: one paragraph naming what Part 1 built, the retention amendment, and the harness entries.
- The spec: add `### 4.10 As built (Part 1)` recording: retention per pair inside `append_series` (amends §4.7); `SeriesCatalog` carries `fetches` and the coverage hull, not a row count (amends §4.6, the no-data-scan rule); `Fetch` has no connection state, so a servable fetch source is `Ok` on the discovery lane at open (amends §4.8); the identities request is `Request::Identities` (amends §5.3/§5.5's `Catalogue` naming); `FetchWorker` reports a fetch's own outcome to a sink that submits to the ingest runner, so the append runs on the ingest thread (clarifies §5.4); coverage subtraction runs on the service thread through the reader connection.

- [ ] **Step 4: Full verification**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo check -p geode-shell --features test-support --all-targets && cargo bench --workspace --no-run && zsh scripts/mutation-check.sh --anchors-only`
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add scripts/mutation-check.sh crates/geode-data/benches crates/geode-data/Cargo.toml docs CLAUDE.md
git commit -m "docs+harness: timeseries Part 1 — entries, append bench, as-built"
```

Then finish the branch per `superpowers:finishing-a-development-branch`.

---

## Self-review notes

- **Spec coverage.** §4.2 declaration → Task 1; §4.3 validation → Task 1; §4.4 storage and append → Task 4; §4.5 as-of → no code in Part 1 (the query is Part 2), the storage shape it needs is Task 4; §4.6 coverage and catalog → Tasks 5, 8; §4.7 retention → Task 6 (amended); §4.8 health → Tasks 8; §4.9 demo database → Task 9; §5.1 config → Task 2; §5.2 traits → Task 3; §5.3 request → Task 8; §5.4 pipeline → Tasks 7, 8; §5.5 catalogue → Task 8; §5.6 demo adapter → Task 9; §11.2 harness → Task 10; §11.3 bench (the append half) → Task 10.
- **Deliberately not in Part 1:** `Request::Series`, `Delivery::*`, any shell change, `[timeseries] default_source`.
- **Type consistency:** `Span` is `geode_data::store::series::Span` everywhere (`SeriesJob`, `FetchOutcome`, `coverage`, `missing_spans`); `SeriesRows` is `geode_data::adapter::SeriesRows` everywhere; the load-lane key string is built as `format!("{identity}@{source}")` at the three sites named in Task 8 step 5.
