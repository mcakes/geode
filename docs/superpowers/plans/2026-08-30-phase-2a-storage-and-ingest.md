# Phase 2a — Storage and Ingest Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the desk's risk CSVs into a persistent, generation-stamped
DuckDB database — correctly split by measure grain, sentinel-gated,
priority-ordered, and retention-swept — with no query path yet.

**Architecture:** `geode-core` gains the declarative schema vocabulary
(grains, columns, measures) that everything downstream reads. `geode-data`
gains a `Store` owning one on-disk DuckDB database, a CSV adapter whose
readiness comes from `.done` sentinels, and an ingest runner that works a
priority-ordered plan across a bounded worker pool. `geode-demo-data` is
rebuilt to emit a realistic source *directory* so every ingest behaviour is
exercised by fixtures rather than mocks.

**Tech Stack:** Rust 2024 edition, stable toolchain. `duckdb` 1.10505 with
the `bundled` feature (compiles DuckDB from C++ source). `serde`/`serde_json`
for sentinels. `criterion` 0.8.2 for benchmarks. No async runtime — ingest is
OS threads and channels.

**Spec:** `docs/superpowers/specs/2026-08-30-geode-phase-2-data-design.md`

## Global Constraints

- **Layering:** `geode-data` never depends on `geode-shell` and vice versa.
  `geode-data` is the only crate permitted to open a file or socket.
- **Every new lib/bin target needs `bench = false`**; every `[[bench]]`
  target needs `harness = false`. Workspace-wide invariant.
- **CI runs `cargo fmt --check`, `cargo clippy --workspace --all-targets -D
  warnings`, `cargo test --workspace`, and `cargo bench --workspace
  --no-run` on both macOS and Windows.** Both platforms must build.
- **Performance is a per-task acceptance criterion, not a later pass**
  (PHILOSOPHY §6). Hot paths allocation-free; no row-object
  materialization anywhere; the grain split runs as SQL inside DuckDB, never
  by pulling rows into Rust.
- **Ingest may never drop a foreground frame** (spec §7.1). Nothing in this
  plan runs on the UI thread.
- **Failures degrade, never panic and never clobber** (spec §5.7). A failed
  load leaves `live` untouched.
- **Grain vocabulary is fixed** (spec §3.3): common key `K = (book, lhu,
  position_ref, counterparty)`; `Instrument = K + instrument_ref`;
  `Underlying = K + instrument_ref + underlying_ref`; `UnderlyingPair = K +
  instrument_ref + underlying_ref + underlying2_ref`.

## File Structure

**`geode-core`** — schema vocabulary, no I/O:

- `src/schema/mod.rs` — `SchemaSpec`, `DatasetSpec`, parsing from a
  `MergedDoc`.
- `src/schema/grain.rs` — `Grain` and its key columns.
- `src/schema/column.rs` — `ColumnSpec`, `ColumnRole`, `ColumnType`,
  `Aggregate`.

**`geode-data`** — the only crate that touches the filesystem:

- `src/lib.rs` — crate root, re-exports.
- `src/health.rs` — `Health`, the degradation vocabulary.
- `src/store/mod.rs` — `Store`: owns the `Database`, hands out connections.
- `src/store/ddl.rs` — table DDL generated from a `DatasetSpec`.
- `src/store/catalog.rs` — `file_generations` bookkeeping and freshness.
- `src/store/publish.rs` — the per-file publish transaction.
- `src/store/retention.rs` — the sweeper and checkpoint scheduling.
- `src/source/mod.rs` — `SourceSpec` and readiness strategy.
- `src/source/sentinel.rs` — permissive `.done` JSON parsing.
- `src/source/discovery.rs` — globbing, readiness, change detection.
- `src/ingest/mod.rs` — re-exports.
- `src/ingest/split.rs` — grain split SQL and conflict detection.
- `src/ingest/load.rs` — the per-file load pipeline.
- `src/ingest/plan.rs` — the priority ladder.
- `src/ingest/runner.rs` — worker pool, preemptible backfill, events.
- `benches/ingest.rs` — ingest throughput and cold-start benchmarks.

**`geode-demo-data`** — rebuilt for the real grain:

- `src/lib.rs` — re-exports.
- `src/model.rs` — struct-of-arrays batch at the source file's grain.
- `src/generate.rs` — deterministic seeded generation.
- `src/emit.rs` — writes a source directory: CSVs plus `.done` sentinels.

---

### Task 1: Grain and schema vocabulary

**Files:**
- Create: `crates/geode-core/src/schema/mod.rs`
- Create: `crates/geode-core/src/schema/grain.rs`
- Create: `crates/geode-core/src/schema/column.rs`
- Modify: `crates/geode-core/src/lib.rs`
- Modify: `crates/geode-core/src/config/merge.rs:17-21`

**Interfaces:**
- Consumes: `geode_core::config::{MergedDoc, Diagnostic, Severity}`.
- Produces: `Grain`, `ColumnSpec`, `ColumnRole`, `ColumnType`, `Aggregate`,
  `DatasetSpec`, `SchemaSpec`, and `SchemaSpec::from_doc(&MergedDoc) ->
  (SchemaSpec, Vec<Diagnostic>)`. Every later task reads schema through
  these.

- [ ] **Step 1: Write the failing grain test**

Create `crates/geode-core/src/schema/grain.rs` with only this test module at
the bottom of an otherwise empty file:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_columns_nest_from_coarse_to_fine() {
        assert_eq!(Grain::Position.key_columns(), &["book", "lhu", "position_ref", "counterparty"]);
        assert_eq!(
            Grain::UnderlyingPair.key_columns(),
            &["book", "lhu", "position_ref", "counterparty", "instrument_ref", "underlying_ref", "underlying2_ref"]
        );
        // Each grain's key is a prefix-extension of the coarser one.
        for (coarse, fine) in [
            (Grain::Position, Grain::Instrument),
            (Grain::Instrument, Grain::Underlying),
            (Grain::Underlying, Grain::UnderlyingPair),
        ] {
            assert!(fine.key_columns().starts_with(coarse.key_columns()));
            assert!(coarse < fine, "Ord must read coarse < fine");
        }
    }

    #[test]
    fn table_names_are_stable() {
        assert_eq!(Grain::Position.table(), "measures_position");
        assert_eq!(Grain::UnderlyingPair.table(), "measures_underlying_pair");
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p geode-core grain`
Expected: FAIL — `cannot find type Grain in this scope`.

- [ ] **Step 3: Implement `Grain`**

Prepend to `crates/geode-core/src/schema/grain.rs`:

```rust
//! Measure grain (spec §3.2). Ord reads coarse < fine: a coarser grain's
//! key is a prefix of every finer one's, which is what makes
//! attributability decidable from the schema alone (spec §6.3).

/// The identity columns a measure is keyed by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Grain {
    Position,
    Instrument,
    Underlying,
    UnderlyingPair,
}

const K: [&str; 4] = ["book", "lhu", "position_ref", "counterparty"];
const K_INSTRUMENT: [&str; 5] = ["book", "lhu", "position_ref", "counterparty", "instrument_ref"];
const K_UNDERLYING: [&str; 6] = [
    "book", "lhu", "position_ref", "counterparty", "instrument_ref", "underlying_ref",
];
const K_PAIR: [&str; 7] = [
    "book", "lhu", "position_ref", "counterparty", "instrument_ref", "underlying_ref",
    "underlying2_ref",
];

impl Grain {
    pub const ALL: [Grain; 4] = [
        Grain::Position,
        Grain::Instrument,
        Grain::Underlying,
        Grain::UnderlyingPair,
    ];

    pub fn key_columns(self) -> &'static [&'static str] {
        match self {
            Grain::Position => &K,
            Grain::Instrument => &K_INSTRUMENT,
            Grain::Underlying => &K_UNDERLYING,
            Grain::UnderlyingPair => &K_PAIR,
        }
    }

    pub fn table(self) -> &'static str {
        match self {
            Grain::Position => "measures_position",
            Grain::Instrument => "measures_instrument",
            Grain::Underlying => "measures_underlying",
            Grain::UnderlyingPair => "measures_underlying_pair",
        }
    }

    pub fn parse(s: &str) -> Option<Grain> {
        match s {
            "position" => Some(Grain::Position),
            "instrument" => Some(Grain::Instrument),
            "underlying" => Some(Grain::Underlying),
            "underlying_pair" => Some(Grain::UnderlyingPair),
            _ => None,
        }
    }
}
```

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test -p geode-core grain`
Expected: PASS (2 tests).

- [ ] **Step 5: Write the failing column-spec test**

Create `crates/geode-core/src/schema/column.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_name_falls_back_to_canonical_name() {
        let mapped = ColumnSpec {
            name: "delta01".into(),
            source_name: Some("Delta01".into()),
            ..ColumnSpec::test_default()
        };
        let plain = ColumnSpec { name: "npv".into(), ..ColumnSpec::test_default() };
        assert_eq!(mapped.source_name(), "Delta01");
        assert_eq!(plain.source_name(), "npv");
    }

    #[test]
    fn measure_grain_is_reachable_from_role() {
        let m = ColumnSpec {
            name: "daily_trading_pnl".into(),
            role: ColumnRole::Measure { grain: Grain::Position, aggregate: Aggregate::Sum },
            ..ColumnSpec::test_default()
        };
        assert_eq!(m.grain(), Some(Grain::Position));
        assert_eq!(ColumnSpec::test_default().grain(), None);
    }
}
```

- [ ] **Step 6: Run it to verify it fails**

Run: `cargo test -p geode-core column`
Expected: FAIL — `cannot find struct ColumnSpec`.

- [ ] **Step 7: Implement `ColumnSpec` and friends**

Prepend to `crates/geode-core/src/schema/column.rs`:

```rust
//! Column declarations (spec §3.6). Grain, requiredness, and textual-search
//! participation are all declared, never inferred — spec §3.5 exists because
//! today's grain assignments are informed guesses.

use super::grain::Grain;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnType {
    Utf8,
    F64,
    I64,
    Date,
    Timestamp,
    Bool,
}

impl ColumnType {
    /// The DuckDB type this maps to in generated DDL.
    pub fn sql(self) -> &'static str {
        match self {
            ColumnType::Utf8 => "VARCHAR",
            ColumnType::F64 => "DOUBLE",
            ColumnType::I64 => "BIGINT",
            ColumnType::Date => "DATE",
            ColumnType::Timestamp => "TIMESTAMP",
            ColumnType::Bool => "BOOLEAN",
        }
    }

    pub fn parse(s: &str) -> Option<ColumnType> {
        match s {
            "utf8" | "string" => Some(ColumnType::Utf8),
            "f64" | "double" => Some(ColumnType::F64),
            "i64" | "bigint" => Some(ColumnType::I64),
            "date" => Some(ColumnType::Date),
            "timestamp" => Some(ColumnType::Timestamp),
            "bool" => Some(ColumnType::Bool),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aggregate {
    Sum,
    Min,
    Max,
    Any,
}

impl Aggregate {
    pub fn sql(self, expr: &str) -> String {
        match self {
            Aggregate::Sum => format!("sum({expr})"),
            Aggregate::Min => format!("min({expr})"),
            Aggregate::Max => format!("max({expr})"),
            Aggregate::Any => format!("any_value({expr})"),
        }
    }

    pub fn parse(s: &str) -> Option<Aggregate> {
        match s {
            "sum" => Some(Aggregate::Sum),
            "min" => Some(Aggregate::Min),
            "max" => Some(Aggregate::Max),
            "any" => Some(Aggregate::Any),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnRole {
    /// Part of a grain key.
    Key,
    /// Dictionary-encoded, scopeable, groupable.
    Dimension,
    /// A number, aggregated at its declared grain.
    Measure { grain: Grain, aggregate: Aggregate },
    /// A non-numeric property carried at a grain (strike, expiry, currency).
    Attribute { grain: Grain },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnSpec {
    pub name: String,
    /// The name in the source file, when it differs (spec §5.1 column map).
    pub source_name: Option<String>,
    pub ty: ColumnType,
    /// Absent-and-required is a health warning; absent-and-optional is
    /// expected and silent (spec §3.6).
    pub required: bool,
    /// Participates in the global text filter (spec §4.1).
    pub textual: bool,
    pub role: ColumnRole,
}

impl ColumnSpec {
    pub fn source_name(&self) -> &str {
        self.source_name.as_deref().unwrap_or(&self.name)
    }

    pub fn grain(&self) -> Option<Grain> {
        match self.role {
            ColumnRole::Measure { grain, .. } | ColumnRole::Attribute { grain } => Some(grain),
            ColumnRole::Key | ColumnRole::Dimension => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn test_default() -> ColumnSpec {
        ColumnSpec {
            name: String::new(),
            source_name: None,
            ty: ColumnType::F64,
            required: true,
            textual: false,
            role: ColumnRole::Dimension,
        }
    }
}
```

- [ ] **Step 8: Run it to verify it passes**

Run: `cargo test -p geode-core column`
Expected: PASS (2 tests).

- [ ] **Step 9: Write the failing schema-parse test**

Create `crates/geode-core/src/schema/mod.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LayerDoc, merge_docs};

    fn doc(text: &str) -> crate::config::MergedDoc {
        merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()])
    }

    const SAMPLE: &str = r#"
[risk_snapshot.columns.book]
type = "utf8"
role = "dimension"
textual = true

[risk_snapshot.columns.delta01]
type = "f64"
role = "measure"
grain = "underlying"
source_name = "Delta01"

[risk_snapshot.columns.daily_trading_pnl]
type = "f64"
role = "measure"
grain = "position"
source_name = "DailyTradingPNL"

[risk_snapshot.columns.cross_gamma02]
type = "f64"
role = "measure"
grain = "underlying_pair"
source_name = "CrossGamma02"
required = false
"#;

    #[test]
    fn parses_columns_with_grain_and_source_names() {
        let (schema, diags) = SchemaSpec::from_doc(&doc(SAMPLE));
        assert!(diags.is_empty(), "{diags:?}");
        let ds = schema.dataset("risk_snapshot").expect("dataset");
        assert_eq!(ds.column("delta01").unwrap().source_name(), "Delta01");
        assert_eq!(ds.column("delta01").unwrap().grain(), Some(Grain::Underlying));
        assert!(ds.column("book").unwrap().textual);
        assert!(ds.column("delta01").unwrap().required, "required defaults true");
        assert!(!ds.column("cross_gamma02").unwrap().required);
    }

    #[test]
    fn grains_are_the_distinct_measure_grains_coarse_first() {
        let (schema, _) = SchemaSpec::from_doc(&doc(SAMPLE));
        let ds = schema.dataset("risk_snapshot").unwrap();
        assert_eq!(
            ds.grains(),
            vec![Grain::Position, Grain::Underlying, Grain::UnderlyingPair]
        );
        let at_underlying: Vec<_> =
            ds.measures_at(Grain::Underlying).map(|c| c.name.as_str()).collect();
        assert_eq!(at_underlying, vec!["delta01"]);
    }

    #[test]
    fn bad_grain_is_a_diagnostic_not_a_panic() {
        let (schema, diags) = SchemaSpec::from_doc(&doc(
            "[risk.columns.x]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"galaxy\"\n",
        ));
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("galaxy"), "{}", diags[0].message);
        assert!(schema.dataset("risk").unwrap().column("x").is_none());
    }
}
```

- [ ] **Step 10: Run it to verify it fails**

Run: `cargo test -p geode-core schema`
Expected: FAIL — `cannot find struct SchemaSpec`.

- [ ] **Step 11: Implement `SchemaSpec`**

Prepend to `crates/geode-core/src/schema/mod.rs`:

```rust
//! The declared shape of the desk's data (spec §3). Parsed from the
//! `datasets` config doc; every parse failure degrades to a Diagnostic and
//! skips the offending column, never panics (spec §5.7, config §8).

mod column;
mod grain;

pub use column::{Aggregate, ColumnRole, ColumnSpec, ColumnType};
pub use grain::Grain;

use crate::config::{Diagnostic, MergedDoc, Severity};

#[derive(Debug, Clone, Default)]
pub struct DatasetSpec {
    pub name: String,
    pub columns: Vec<ColumnSpec>,
}

impl DatasetSpec {
    pub fn column(&self, name: &str) -> Option<&ColumnSpec> {
        self.columns.iter().find(|c| c.name == name)
    }

    /// Distinct measure grains present, coarse first.
    pub fn grains(&self) -> Vec<Grain> {
        let mut out: Vec<Grain> = self
            .columns
            .iter()
            .filter(|c| matches!(c.role, ColumnRole::Measure { .. }))
            .filter_map(|c| c.grain())
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    pub fn measures_at(&self, grain: Grain) -> impl Iterator<Item = &ColumnSpec> {
        self.columns.iter().filter(move |c| {
            matches!(c.role, ColumnRole::Measure { .. }) && c.grain() == Some(grain)
        })
    }

    pub fn attributes_at(&self, grain: Grain) -> impl Iterator<Item = &ColumnSpec> {
        self.columns.iter().filter(move |c| {
            matches!(c.role, ColumnRole::Attribute { .. }) && c.grain() == Some(grain)
        })
    }

    pub fn textual_columns(&self) -> impl Iterator<Item = &ColumnSpec> {
        self.columns.iter().filter(|c| c.textual)
    }
}

#[derive(Debug, Clone, Default)]
pub struct SchemaSpec {
    pub datasets: Vec<DatasetSpec>,
}

impl SchemaSpec {
    pub fn dataset(&self, name: &str) -> Option<&DatasetSpec> {
        self.datasets.iter().find(|d| d.name == name)
    }

    pub fn from_doc(doc: &MergedDoc) -> (SchemaSpec, Vec<Diagnostic>) {
        let mut out = SchemaSpec::default();
        let mut diags = Vec::new();
        for (ds_name, ds_value) in &doc.value {
            let mut dataset = DatasetSpec {
                name: ds_name.clone(),
                columns: Vec::new(),
            };
            let Some(cols) = ds_value.get("columns").and_then(|v| v.as_table()) else {
                diags.push(note(format!("dataset '{ds_name}': no [columns] table")));
                out.datasets.push(dataset);
                continue;
            };
            for (col_name, col_value) in cols {
                match parse_column(ds_name, col_name, col_value) {
                    Ok(spec) => dataset.columns.push(spec),
                    Err(d) => diags.push(d),
                }
            }
            out.datasets.push(dataset);
        }
        (out, diags)
    }
}

fn note(message: String) -> Diagnostic {
    Diagnostic {
        severity: Severity::Warning,
        layer: None,
        file: None,
        message,
    }
}

fn parse_column(
    ds: &str,
    name: &str,
    value: &toml::Value,
) -> Result<ColumnSpec, Diagnostic> {
    let bad = |m: String| note(format!("dataset '{ds}' column '{name}': {m}"));
    let table = value.as_table().ok_or_else(|| bad("not a table".into()))?;

    let ty_str = table
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| bad("missing 'type'".into()))?;
    let ty = ColumnType::parse(ty_str).ok_or_else(|| bad(format!("unknown type '{ty_str}'")))?;

    let role_str = table
        .get("role")
        .and_then(|v| v.as_str())
        .ok_or_else(|| bad("missing 'role'".into()))?;

    let grain_of = |table: &toml::Table| -> Result<Grain, Diagnostic> {
        let g = table
            .get("grain")
            .and_then(|v| v.as_str())
            .ok_or_else(|| bad("missing 'grain'".into()))?;
        Grain::parse(g).ok_or_else(|| bad(format!("unknown grain '{g}'")))
    };

    let role = match role_str {
        "key" => ColumnRole::Key,
        "dimension" => ColumnRole::Dimension,
        "attribute" => ColumnRole::Attribute { grain: grain_of(table)? },
        "measure" => {
            let agg_str = table.get("aggregate").and_then(|v| v.as_str()).unwrap_or("sum");
            let aggregate = Aggregate::parse(agg_str)
                .ok_or_else(|| bad(format!("unknown aggregate '{agg_str}'")))?;
            ColumnRole::Measure { grain: grain_of(table)?, aggregate }
        }
        other => return Err(bad(format!("unknown role '{other}'"))),
    };

    Ok(ColumnSpec {
        name: name.to_string(),
        source_name: table.get("source_name").and_then(|v| v.as_str()).map(str::to_string),
        ty,
        required: table.get("required").and_then(|v| v.as_bool()).unwrap_or(true),
        textual: table.get("textual").and_then(|v| v.as_bool()).unwrap_or(false),
        role,
    })
}
```

- [ ] **Step 12: Register the module and make `datasets`/`sources` atomic docs**

In `crates/geode-core/src/lib.rs`, add after the existing `pub mod config;`:

```rust
pub mod schema;
```

In `crates/geode-core/src/config/merge.rs`, extend `atomic_depth` so a
dataset or source is overridden whole-object by name, like views and
groupings already are:

```rust
fn atomic_depth(doc_name: &str) -> Option<u32> {
    match doc_name {
        "views" | "layouts" | "groupings" | "scopes" | "datasets" | "sources" => Some(1),
        _ => None,
    }
}
```

- [ ] **Step 13: Run the full core suite**

Run: `cargo test -p geode-core && cargo clippy -p geode-core --all-targets -- -D warnings`
Expected: PASS, no warnings.

- [ ] **Step 14: Commit**

```bash
git add crates/geode-core/src/schema crates/geode-core/src/lib.rs crates/geode-core/src/config/merge.rs
git commit -m "feat(core): declarative schema vocabulary with measure grain

Grain, ColumnSpec and SchemaSpec parsed from the datasets config doc.
Grain's Ord reads coarse < fine and each key is a prefix-extension of the
coarser one, which is what lets attributability be decided from the schema
alone (spec §6.3). Parse failures degrade to Diagnostics per spec §5.7.

Registers datasets/sources as atomic-depth-1 config docs so a dataset is
overridden whole-object by name rather than deep-merged."
```

---

### Task 2: Demo data — model and deterministic generation

**Files:**
- Create: `crates/geode-demo-data/src/model.rs`
- Create: `crates/geode-demo-data/src/generate.rs`
- Rewrite: `crates/geode-demo-data/src/lib.rs`
- Modify: `crates/geode-demo-data/benches/generate.rs`

**Interfaces:**
- Consumes: nothing from earlier tasks (deliberately dependency-free).
- Produces: `GeneratorConfig { rows, seed, business_dates }`, `RiskBatch`
  (SoA at the source file's grain), `generate(&GeneratorConfig) ->
  RiskBatch`, and `RiskBatch::len()`. Task 3 consumes `RiskBatch`.

**Design note — the file's row grain.** Per spec §10.3 this is an open
question; the generator commits to the documented reading: one row per
*ordered* underlying pair. An instrument over `[SPX, RUT, NDX]` emits six
rows — `(SPX,RUT) (SPX,NDX) (RUT,SPX) (RUT,NDX) (NDX,SPX) (NDX,RUT)` — with
single-underlying greeks repeated across the rows sharing `underlying_ref`,
and cross gamma specific to the pair. That is exactly the shape the grain
split and pair canonicalization must handle, so the fixtures exercise both.

- [ ] **Step 1: Write the failing model/generation tests**

Replace `crates/geode-demo-data/src/lib.rs` entirely with:

```rust
//! Deterministic synthetic risk data at the desk's real grain (spec §9.1).
//! Seeded: same config always yields identical data. Struct-of-arrays per
//! PHILOSOPHY §6 — no row objects.

mod generate;
mod model;

pub use generate::{GeneratorConfig, generate};
pub use model::RiskBatch;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn cfg(rows: usize) -> GeneratorConfig {
        GeneratorConfig { rows, seed: 42, business_dates: 2 }
    }

    #[test]
    fn same_seed_yields_identical_data() {
        let a = generate(&cfg(2_000));
        let b = generate(&cfg(2_000));
        assert_eq!(a.position_ref, b.position_ref);
        assert_eq!(a.delta01, b.delta01);
        assert_eq!(a.cross_gamma02, b.cross_gamma02);
    }

    #[test]
    fn emits_ordered_pairs_per_instrument() {
        let b = generate(&cfg(5_000));
        // Every row names two distinct underlyings.
        for i in 0..b.len() {
            assert_ne!(b.underlying_ref[i], b.underlying2_ref[i], "row {i}");
        }
        // At least one instrument has three underlyings, hence six rows.
        let mut per_instrument: std::collections::HashMap<&str, HashSet<&str>> =
            Default::default();
        for i in 0..b.len() {
            per_instrument
                .entry(&b.instrument_ref[i])
                .or_default()
                .insert(&b.underlying_ref[i]);
        }
        assert!(
            per_instrument.values().any(|u| u.len() >= 3),
            "expected at least one worst-of with 3+ underlyings"
        );
    }

    #[test]
    fn coarse_measures_repeat_identically_within_their_grain() {
        let b = generate(&cfg(5_000));
        // NPV is instrument-grain: identical on every row of an instrument.
        let mut seen: std::collections::HashMap<&str, f64> = Default::default();
        for i in 0..b.len() {
            let e = seen.entry(&b.instrument_ref[i]).or_insert(b.npv[i]);
            assert_eq!(*e, b.npv[i], "npv varies within instrument {}", b.instrument_ref[i]);
        }
    }

    #[test]
    fn single_underlying_greeks_repeat_across_a_row_s_pairs() {
        let b = generate(&cfg(5_000));
        let mut seen: std::collections::HashMap<(&str, &str), f64> = Default::default();
        for i in 0..b.len() {
            let key = (b.instrument_ref[i].as_str(), b.underlying_ref[i].as_str());
            let e = seen.entry(key).or_insert(b.delta01[i]);
            assert_eq!(*e, b.delta01[i], "delta01 varies within (instrument, underlying)");
        }
    }

    #[test]
    fn cross_gamma_is_symmetric_across_orderings() {
        let b = generate(&cfg(5_000));
        let mut seen: std::collections::HashMap<(&str, String), f64> = Default::default();
        for i in 0..b.len() {
            let (u1, u2) = (&b.underlying_ref[i], &b.underlying2_ref[i]);
            let canon = if u1 <= u2 { format!("{u1}|{u2}") } else { format!("{u2}|{u1}") };
            let e = seen.entry((b.instrument_ref[i].as_str(), canon)).or_insert(b.cross_gamma02[i]);
            assert_eq!(*e, b.cross_gamma02[i], "cross gamma differs between orderings");
        }
    }

    #[test]
    fn dimensions_have_realistic_bounded_cardinality() {
        let b = generate(&cfg(20_000));
        let books: HashSet<_> = b.book.iter().collect();
        let underlyings: HashSet<_> = b.underlying_ref.iter().collect();
        let lhus: HashSet<_> = b.lhu.iter().collect();
        assert!((2..=20).contains(&books.len()), "books: {}", books.len());
        assert!(underlyings.len() <= 10);
        assert!(lhus.len() > books.len(), "each book should hold several LHUs");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p geode-demo-data`
Expected: FAIL — unresolved modules `generate`, `model`.

- [ ] **Step 3: Implement the SoA model**

Create `crates/geode-demo-data/src/model.rs`:

```rust
//! Struct-of-arrays batch at the source file's grain: one row per ordered
//! underlying pair per instrument (spec §10.3's documented reading).

/// One Vec per column, index = row. Column names are the canonical
/// (snake_case) names; `emit` maps them to the source's header spelling.
#[derive(Debug, Default)]
pub struct RiskBatch {
    // Identity
    pub business_date: Vec<String>,
    pub book: Vec<String>,
    pub lhu: Vec<String>,
    pub position_ref: Vec<String>,
    pub instrument_ref: Vec<String>,
    pub underlying_ref: Vec<String>,
    pub underlying2_ref: Vec<String>,
    pub counterparty: Vec<String>,
    // Instrument reference attributes
    pub strike: Vec<f64>,
    pub expiry: Vec<String>,
    pub currency: Vec<String>,
    pub model_code: Vec<String>,
    // Underlying-grain measures
    pub delta01: Vec<f64>,
    pub delta02: Vec<f64>,
    pub delta05: Vec<f64>,
    pub gamma01: Vec<f64>,
    pub gamma02: Vec<f64>,
    pub gamma05: Vec<f64>,
    pub vega01: Vec<f64>,
    pub normalized_vega01: Vec<f64>,
    pub skew01: Vec<f64>,
    pub rho010: Vec<f64>,
    pub rho_rfr010: Vec<f64>,
    pub rho_ois010: Vec<f64>,
    // Pair-grain measures
    pub cross_gamma02: Vec<f64>,
    pub cross_gamma05: Vec<f64>,
    // Instrument-grain measures
    pub npv: Vec<f64>,
    pub daily_pnl: Vec<f64>,
    pub daily_m2m_pnl: Vec<f64>,
    pub daily_fx_pnl: Vec<f64>,
    pub clean_theta_business_day: Vec<f64>,
    pub realized_theta: Vec<f64>,
    // Position-grain measures
    pub daily_trading_pnl: Vec<f64>,
    pub sc: Vec<f64>,
}

impl RiskBatch {
    pub fn len(&self) -> usize {
        self.position_ref.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Canonical column order. `emit` writes headers and rows in this order,
    /// and the `_USD` twin of each measure is written immediately after it.
    pub const IDENTITY: &'static [&'static str] = &[
        "business_date", "book", "lhu", "position_ref", "instrument_ref",
        "underlying_ref", "underlying2_ref", "counterparty", "strike", "expiry",
        "currency", "model_code",
    ];

    pub const UNDERLYING_MEASURES: &'static [&'static str] = &[
        "delta01", "delta02", "delta05", "gamma01", "gamma02", "gamma05",
        "vega01", "normalized_vega01", "skew01", "rho010", "rho_rfr010",
        "rho_ois010",
    ];

    pub const PAIR_MEASURES: &'static [&'static str] = &["cross_gamma02", "cross_gamma05"];

    pub const INSTRUMENT_MEASURES: &'static [&'static str] = &[
        "npv", "daily_pnl", "daily_m2m_pnl", "daily_fx_pnl",
        "clean_theta_business_day", "realized_theta",
    ];

    pub const POSITION_MEASURES: &'static [&'static str] = &["daily_trading_pnl", "sc"];

    /// Read a measure column by canonical name; used by `emit` so the
    /// writer stays a loop over names rather than 30 hand-written fields.
    pub fn measure(&self, name: &str) -> &[f64] {
        match name {
            "delta01" => &self.delta01,
            "delta02" => &self.delta02,
            "delta05" => &self.delta05,
            "gamma01" => &self.gamma01,
            "gamma02" => &self.gamma02,
            "gamma05" => &self.gamma05,
            "vega01" => &self.vega01,
            "normalized_vega01" => &self.normalized_vega01,
            "skew01" => &self.skew01,
            "rho010" => &self.rho010,
            "rho_rfr010" => &self.rho_rfr010,
            "rho_ois010" => &self.rho_ois010,
            "cross_gamma02" => &self.cross_gamma02,
            "cross_gamma05" => &self.cross_gamma05,
            "npv" => &self.npv,
            "daily_pnl" => &self.daily_pnl,
            "daily_m2m_pnl" => &self.daily_m2m_pnl,
            "daily_fx_pnl" => &self.daily_fx_pnl,
            "clean_theta_business_day" => &self.clean_theta_business_day,
            "realized_theta" => &self.realized_theta,
            "daily_trading_pnl" => &self.daily_trading_pnl,
            "sc" => &self.sc,
            other => panic!("unknown measure column '{other}'"),
        }
    }

    /// Read an identity column by canonical name. `strike` is numeric and is
    /// formatted by the caller; every other identity column is a string.
    pub fn identity(&self, name: &str) -> &[String] {
        match name {
            "business_date" => &self.business_date,
            "book" => &self.book,
            "lhu" => &self.lhu,
            "position_ref" => &self.position_ref,
            "instrument_ref" => &self.instrument_ref,
            "underlying_ref" => &self.underlying_ref,
            "underlying2_ref" => &self.underlying2_ref,
            "counterparty" => &self.counterparty,
            "expiry" => &self.expiry,
            "currency" => &self.currency,
            "model_code" => &self.model_code,
            other => panic!("'{other}' is not a string identity column"),
        }
    }
}
```

- [ ] **Step 4: Implement generation**

Create `crates/geode-demo-data/src/generate.rs`:

```rust
//! Seeded generation. Structure first (desks → books → LHUs → positions →
//! instruments → underlyings), then measures assigned *at their grain* so
//! coarse values repeat exactly, which is what the ingest grain split and
//! conflict detector are tested against.

use crate::model::RiskBatch;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

pub struct GeneratorConfig {
    /// Target row count. Structure is generated until this is reached.
    pub rows: usize,
    pub seed: u64,
    /// How many consecutive business dates to spread generations over.
    pub business_dates: usize,
}

impl Default for GeneratorConfig {
    fn default() -> Self {
        Self { rows: 100_000, seed: 42, business_dates: 3 }
    }
}

const UNDERLYINGS: &[&str] = &[
    "SPX", "SX5E", "NKY", "UKX", "NDX", "RTY", "DAX", "SMI", "HSI", "KOSPI2",
];
const CURRENCIES: &[&str] = &["USD", "EUR", "JPY", "GBP"];
const MODEL_CODES: &[&str] = &["EURP", "AMRP", "VSWP", "AUTO", "CLIQ", "BARR", "DIGI", "VANL"];
const COUNTERPARTIES: &[&str] = &["CPTY_A", "CPTY_B", "CPTY_C", "CPTY_D"];
const BOOK_COUNT: usize = 20;
const LHUS_PER_BOOK: usize = 4;

pub fn generate(config: &GeneratorConfig) -> RiskBatch {
    let mut rng = StdRng::seed_from_u64(config.seed);
    let mut b = RiskBatch::default();
    let mut position_seq: u64 = 0;

    'outer: for date_idx in 0..config.business_dates.max(1) {
        let business_date = format!("2026-08-{:02}", 24 + date_idx);
        for book_idx in 0..BOOK_COUNT {
            let book = format!("BK{book_idx:03}");
            for lhu_idx in 0..LHUS_PER_BOOK {
                let lhu = format!("{book}_LHU{lhu_idx}");
                // A handful of positions per LHU, each with 1-3 legs.
                for _ in 0..8 {
                    position_seq += 1;
                    let position_ref = format!("POS{position_seq:07}");
                    let counterparty =
                        COUNTERPARTIES[rng.random_range(0..COUNTERPARTIES.len())].to_string();

                    // Position-grain measures: one value, repeated on every row.
                    let daily_trading_pnl = rng.random_range(-250_000.0..250_000.0);
                    let sc = rng.random_range(0.0..80_000.0);

                    let legs = rng.random_range(1..=3);
                    for leg in 0..legs {
                        let instrument_ref = format!("{position_ref}{}", (b'a' + leg as u8) as char);

                        // Instrument reference attributes and instrument-grain
                        // measures: one value each, repeated on every row.
                        let strike = (rng.random_range(50.0..150.0f64) * 100.0).round() / 100.0;
                        let expiry = format!("2027-{:02}-15", rng.random_range(1..=12));
                        let currency = CURRENCIES[rng.random_range(0..CURRENCIES.len())].to_string();
                        let model_code =
                            MODEL_CODES[rng.random_range(0..MODEL_CODES.len())].to_string();
                        let npv = rng.random_range(-5_000_000.0..5_000_000.0);
                        let daily_pnl = rng.random_range(-500_000.0..500_000.0);
                        let daily_m2m_pnl = daily_pnl * 0.8;
                        let daily_fx_pnl = daily_pnl * 0.2;
                        let clean_theta = rng.random_range(-30_000.0..0.0);
                        let realized_theta = clean_theta * 0.9;

                        // 2 or 3 underlyings: mono-underlying products still
                        // carry currency risk, so 2 is the floor (spec §3.1).
                        let n_underlying = if rng.random_range(0..10) == 0 { 3 } else { 2 };
                        let mut unders: Vec<&str> = Vec::with_capacity(n_underlying);
                        while unders.len() < n_underlying {
                            let u = UNDERLYINGS[rng.random_range(0..UNDERLYINGS.len())];
                            if !unders.contains(&u) {
                                unders.push(u);
                            }
                        }

                        // Underlying-grain measures: one value per underlying,
                        // repeated across that underlying's pair rows.
                        let per_underlying: Vec<[f64; 12]> = unders
                            .iter()
                            .map(|_| {
                                [
                                    rng.random_range(-100_000.0..100_000.0), // delta01
                                    rng.random_range(-100_000.0..100_000.0), // delta02
                                    rng.random_range(-100_000.0..100_000.0), // delta05
                                    rng.random_range(-5_000.0..5_000.0),     // gamma01
                                    rng.random_range(-5_000.0..5_000.0),     // gamma02
                                    rng.random_range(-5_000.0..5_000.0),     // gamma05
                                    rng.random_range(-50_000.0..50_000.0),   // vega01
                                    rng.random_range(-5_000.0..5_000.0),     // normalized_vega01
                                    rng.random_range(-8_000.0..8_000.0),     // skew01
                                    rng.random_range(-20_000.0..20_000.0),   // rho010
                                    rng.random_range(-20_000.0..20_000.0),   // rho_rfr010
                                    rng.random_range(-20_000.0..20_000.0),   // rho_ois010
                                ]
                            })
                            .collect();

                        // Pair-grain measures: keyed by the *canonical* pair so
                        // both orderings carry the same value (spec §3.3).
                        let mut pair_values: Vec<((usize, usize), [f64; 2])> = Vec::new();
                        for i in 0..unders.len() {
                            for j in (i + 1)..unders.len() {
                                pair_values.push((
                                    (i, j),
                                    [
                                        rng.random_range(-2_000.0..2_000.0),
                                        rng.random_range(-2_000.0..2_000.0),
                                    ],
                                ));
                            }
                        }
                        let pair_value = |i: usize, j: usize| -> [f64; 2] {
                            let key = if i < j { (i, j) } else { (j, i) };
                            pair_values.iter().find(|(k, _)| *k == key).map(|(_, v)| *v).unwrap()
                        };

                        for i in 0..unders.len() {
                            for j in 0..unders.len() {
                                if i == j {
                                    continue;
                                }
                                let u = per_underlying[i];
                                let p = pair_value(i, j);
                                b.business_date.push(business_date.clone());
                                b.book.push(book.clone());
                                b.lhu.push(lhu.clone());
                                b.position_ref.push(position_ref.clone());
                                b.instrument_ref.push(instrument_ref.clone());
                                b.underlying_ref.push(unders[i].to_string());
                                b.underlying2_ref.push(unders[j].to_string());
                                b.counterparty.push(counterparty.clone());
                                b.strike.push(strike);
                                b.expiry.push(expiry.clone());
                                b.currency.push(currency.clone());
                                b.model_code.push(model_code.clone());
                                b.delta01.push(u[0]);
                                b.delta02.push(u[1]);
                                b.delta05.push(u[2]);
                                b.gamma01.push(u[3]);
                                b.gamma02.push(u[4]);
                                b.gamma05.push(u[5]);
                                b.vega01.push(u[6]);
                                b.normalized_vega01.push(u[7]);
                                b.skew01.push(u[8]);
                                b.rho010.push(u[9]);
                                b.rho_rfr010.push(u[10]);
                                b.rho_ois010.push(u[11]);
                                b.cross_gamma02.push(p[0]);
                                b.cross_gamma05.push(p[1]);
                                b.npv.push(npv);
                                b.daily_pnl.push(daily_pnl);
                                b.daily_m2m_pnl.push(daily_m2m_pnl);
                                b.daily_fx_pnl.push(daily_fx_pnl);
                                b.clean_theta_business_day.push(clean_theta);
                                b.realized_theta.push(realized_theta);
                                b.daily_trading_pnl.push(daily_trading_pnl);
                                b.sc.push(sc);
                                if b.len() >= config.rows {
                                    break 'outer;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    b
}
```

- [ ] **Step 5: Run to verify tests pass**

Run: `cargo test -p geode-demo-data`
Expected: PASS (6 tests).

- [ ] **Step 6: Update the existing benchmark to the new API**

In `crates/geode-demo-data/benches/generate.rs`, replace both
`GeneratorConfig` literals so they carry the new field:

```rust
GeneratorConfig { rows: 100_000, seed: 42, business_dates: 1 }
```

and

```rust
GeneratorConfig { rows: 1_000_000, seed: 42, business_dates: 1 }
```

- [ ] **Step 7: Verify benches still compile and the suite is green**

Run: `cargo bench -p geode-demo-data --no-run && cargo clippy -p geode-demo-data --all-targets -- -D warnings`
Expected: compiles, no warnings.

- [ ] **Step 8: Commit**

```bash
git add crates/geode-demo-data
git commit -m "feat(demo-data): rebuild generator at the desk's real grain

Replaces the flat toy schema with the four-grain structure of spec §3:
desks/books/LHUs/positions/instruments/underlyings, one row per ordered
underlying pair, and measures assigned at their declared grain so coarse
values repeat exactly. That repetition is the fixture the ingest grain
split and conflict detector are tested against.

Cross gamma is generated per canonical pair, so both orderings carry the
same value and canonicalization has something real to collapse."
```

---

### Task 3: Demo data — emit a realistic source directory

**Files:**
- Create: `crates/geode-demo-data/src/emit.rs`
- Modify: `crates/geode-demo-data/src/lib.rs`
- Modify: `crates/geode-demo-data/Cargo.toml`

**Interfaces:**
- Consumes: `RiskBatch`, `generate` from Task 2.
- Produces: `EmitOptions`, `EmittedFile { csv_path, sentinel_path, books,
  rows, columns }`, `EmittedDirectory { files: Vec<EmittedFile> }`, and
  `emit_directory(&RiskBatch, &EmitOptions) -> std::io::Result<EmittedDirectory>`.
  Every ingest test from Task 9 onward builds its fixture with this.

**Why this shape.** The awkwardness is the point (spec §9.1): a book split
across two files, a file carrying two books, a CSV whose sentinel has not
landed, files missing optional columns, and an instrument whose attributes
disagree between books. Every one of those is a behaviour the ingest code
must handle, and generating them is far cheaper than hand-maintaining
fixture files — which §7.4 forbids anyway.

- [ ] **Step 1: Add the serde dependency**

In `crates/geode-demo-data/Cargo.toml`, add to `[dependencies]`:

```toml
serde_json = "1.0.151"
```

- [ ] **Step 2: Write the failing emit tests**

Add to `crates/geode-demo-data/src/lib.rs`, inside the existing
`mod tests`:

```rust
    #[test]
    fn emits_csvs_and_sentinels_with_the_awkward_cases() {
        let dir = tempfile::tempdir().unwrap();
        let batch = generate(&cfg(20_000));
        let out = emit_directory(&batch, &EmitOptions::new(dir.path())).unwrap();

        assert!(out.files.len() >= 4, "expected several files");

        // A book split across two files.
        let split: Vec<_> = out
            .files
            .iter()
            .filter(|f| f.books == vec!["BK000".to_string()])
            .collect();
        assert_eq!(split.len(), 2, "BK000 must be split across two files");

        // A file carrying more than one book.
        assert!(
            out.files.iter().any(|f| f.books.len() > 1),
            "expected a multi-book file"
        );

        // Exactly one CSV without a sentinel (readiness: pending).
        let pending: Vec<_> = out.files.iter().filter(|f| f.sentinel_path.is_none()).collect();
        assert_eq!(pending.len(), 1);

        // Optional columns absent from at least one file.
        assert!(
            out.files.iter().any(|f| !f.columns.iter().any(|c| c == "Skew01")),
            "expected a file missing an optional column"
        );

        for f in &out.files {
            assert!(f.csv_path.exists());
            if let Some(s) = &f.sentinel_path {
                assert!(s.exists());
            }
        }
    }

    #[test]
    fn sentinel_json_carries_source_time_and_columns() {
        let dir = tempfile::tempdir().unwrap();
        let batch = generate(&cfg(5_000));
        let out = emit_directory(&batch, &EmitOptions::new(dir.path())).unwrap();
        let f = out.files.iter().find(|f| f.sentinel_path.is_some()).unwrap();
        let text = std::fs::read_to_string(f.sentinel_path.as_ref().unwrap()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();

        assert!(v["as_of"].as_str().unwrap().starts_with("20"));
        assert_eq!(v["row_count"].as_u64().unwrap() as usize, f.rows);
        let cols: Vec<String> = v["columns"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c.as_str().unwrap().to_string())
            .collect();
        assert_eq!(cols, f.columns);
        assert!(cols.contains(&"Delta01".to_string()), "source spelling, not snake_case");
        assert!(cols.contains(&"Delta01_USD".to_string()));
    }

    #[test]
    fn csv_row_count_matches_the_sentinel() {
        let dir = tempfile::tempdir().unwrap();
        let batch = generate(&cfg(5_000));
        let out = emit_directory(&batch, &EmitOptions::new(dir.path())).unwrap();
        for f in &out.files {
            let text = std::fs::read_to_string(&f.csv_path).unwrap();
            assert_eq!(text.lines().count(), f.rows + 1, "{:?}", f.csv_path);
        }
    }

    #[test]
    fn conflicting_instrument_attributes_are_planted() {
        let dir = tempfile::tempdir().unwrap();
        let batch = generate(&cfg(20_000));
        let opts = EmitOptions::new(dir.path());
        let out = emit_directory(&batch, &opts).unwrap();
        assert!(
            !out.conflicting_instruments.is_empty(),
            "the fixture must plant at least one attribute disagreement"
        );
    }
```

Add `tempfile = "3.27.0"` to `[dev-dependencies]` in
`crates/geode-demo-data/Cargo.toml`, and add to the top of `lib.rs`:

```rust
mod emit;

pub use emit::{EmitOptions, EmittedDirectory, EmittedFile, emit_directory};
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test -p geode-demo-data emit`
Expected: FAIL — unresolved module `emit`.

- [ ] **Step 4: Implement the emitter**

Create `crates/geode-demo-data/src/emit.rs`:

```rust
//! Writes a realistic source directory: per-book CSVs in the source's own
//! column spelling, each with a `.done` JSON sentinel (spec §5.3).
//!
//! No quoting or escaping: every string column draws from fixed, comma-free
//! vocabularies. Revisit if a vocabulary ever grows free-form values.

use crate::model::RiskBatch;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Canonical name -> the spelling the source file uses.
const SOURCE_NAMES: &[(&str, &str)] = &[
    ("business_date", "BusinessDate"),
    ("book", "Book"),
    ("lhu", "LHU"),
    ("position_ref", "PositionRef"),
    ("instrument_ref", "InstrumentRef"),
    ("underlying_ref", "Underlying1Ref"),
    ("underlying2_ref", "Underlying2Ref"),
    ("counterparty", "Counterparty"),
    ("strike", "Strike"),
    ("expiry", "Expiry"),
    ("currency", "Currency"),
    ("model_code", "ModelCode"),
    ("delta01", "Delta01"),
    ("delta02", "Delta02"),
    ("delta05", "Delta05"),
    ("gamma01", "Gamma01"),
    ("gamma02", "Gamma02"),
    ("gamma05", "Gamma05"),
    ("vega01", "Vega01"),
    ("normalized_vega01", "NormalizedVega01"),
    ("skew01", "Skew01"),
    ("rho010", "Rho010"),
    ("rho_rfr010", "RhoRFR010"),
    ("rho_ois010", "RhoOIS010"),
    ("cross_gamma02", "CrossGamma02"),
    ("cross_gamma05", "CrossGamma05"),
    ("npv", "NPV"),
    ("daily_pnl", "DailyPNL"),
    ("daily_m2m_pnl", "DailyM2MPNL"),
    ("daily_fx_pnl", "DailyFXPNL"),
    ("clean_theta_business_day", "CleanThetaBusinessDay"),
    ("realized_theta", "RealizedTheta"),
    ("daily_trading_pnl", "DailyTradingPNL"),
    ("sc", "SC"),
];

/// Measures that also get an FX-converted `_USD` twin (spec §3.4).
const USD_TWINS: &[&str] = &[
    "delta01", "delta02", "delta05", "gamma01", "gamma02", "gamma05", "vega01",
    "normalized_vega01", "skew01", "rho010", "rho_rfr010", "rho_ois010",
    "cross_gamma02", "cross_gamma05", "clean_theta_business_day", "realized_theta",
];

/// Optional columns omitted from some files, so §3.6's tolerance is
/// exercised by fixtures (not every book is run with every greek).
const OMITTED_FROM_SOME_FILES: &[&str] = &["skew01", "rho_ois010"];

fn source_name(canonical: &str) -> &'static str {
    SOURCE_NAMES
        .iter()
        .find(|(c, _)| *c == canonical)
        .map(|(_, s)| *s)
        .unwrap_or_else(|| panic!("no source spelling for '{canonical}'"))
}

pub struct EmitOptions {
    pub root: PathBuf,
    /// Base timestamp; each file's `as_of` is offset from this so per-book
    /// freshness differs, which is what the §4.5 rollup is tested against.
    pub as_of_base: String,
    /// Omit the sentinel for one file, making it "pending" (spec §5.2).
    pub leave_one_pending: bool,
}

impl EmitOptions {
    pub fn new(root: impl Into<PathBuf>) -> EmitOptions {
        EmitOptions {
            root: root.into(),
            as_of_base: "2026-08-30T07:00:00Z".to_string(),
            leave_one_pending: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EmittedFile {
    pub csv_path: PathBuf,
    /// `None` when the sentinel was deliberately withheld.
    pub sentinel_path: Option<PathBuf>,
    pub books: Vec<String>,
    pub rows: usize,
    /// Source-spelled column headers, in file order.
    pub columns: Vec<String>,
    pub as_of: String,
}

#[derive(Debug, Clone)]
pub struct EmittedDirectory {
    pub files: Vec<EmittedFile>,
    /// Instruments whose attributes were deliberately made to disagree
    /// between files, for the §3.5 conflict detector.
    pub conflicting_instruments: Vec<String>,
}

/// Group row indices into files: most books get one file, `BK000` is split
/// across two, and `BK001`+`BK002` share one.
fn file_assignments(batch: &RiskBatch) -> BTreeMap<String, Vec<usize>> {
    let mut by_file: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut split_toggle = false;
    for i in 0..batch.len() {
        let book = &batch.book[i];
        let date = &batch.business_date[i];
        let key = match book.as_str() {
            "BK000" => {
                split_toggle = !split_toggle;
                format!("risk_{date}_BK000_part{}", if split_toggle { 1 } else { 2 })
            }
            "BK001" | "BK002" => format!("risk_{date}_BK001_BK002"),
            other => format!("risk_{date}_{other}"),
        };
        by_file.entry(key).or_default().push(i);
    }
    by_file
}

pub fn emit_directory(
    batch: &RiskBatch,
    opts: &EmitOptions,
) -> std::io::Result<EmittedDirectory> {
    std::fs::create_dir_all(&opts.root)?;
    let assignments = file_assignments(batch);
    let mut files = Vec::new();
    let mut conflicting_instruments = Vec::new();

    for (idx, (stem, rows)) in assignments.iter().enumerate() {
        // Every third file omits the optional columns.
        let omit: &[&str] = if idx % 3 == 2 { OMITTED_FROM_SOME_FILES } else { &[] };
        let columns = header_columns(omit);

        // Plant an attribute disagreement in the second file: the same
        // instrument gets a different model code than elsewhere.
        let plant_conflict = idx == 1;

        let csv_path = opts.root.join(format!("{stem}.csv"));
        let mut out = std::io::BufWriter::new(std::fs::File::create(&csv_path)?);
        writeln!(out, "{}", columns.join(","))?;

        for &i in rows {
            let mut fields: Vec<String> = Vec::with_capacity(columns.len());
            for canonical in canonical_columns(omit) {
                fields.push(field_value(batch, i, &canonical, plant_conflict));
            }
            if plant_conflict && !conflicting_instruments.contains(&batch.instrument_ref[i]) {
                conflicting_instruments.push(batch.instrument_ref[i].clone());
            }
            writeln!(out, "{}", fields.join(","))?;
        }
        out.flush()?;

        let mut books: Vec<String> =
            rows.iter().map(|&i| batch.book[i].clone()).collect();
        books.sort_unstable();
        books.dedup();

        // Stagger source times so per-book freshness differs.
        let as_of = format!(
            "2026-08-30T{:02}:{:02}:00Z",
            7 + (idx as u32 % 8),
            (idx as u32 * 7) % 60
        );

        let withhold = opts.leave_one_pending && idx == assignments.len() - 1;
        let sentinel_path = if withhold {
            None
        } else {
            let p = opts.root.join(format!("{stem}.csv.done"));
            let doc = serde_json::json!({
                "dataset": "risk_snapshot",
                "as_of": as_of,
                "business_date": batch.business_date[rows[0]],
                "books": books,
                "row_count": rows.len(),
                "columns": columns,
            });
            std::fs::write(&p, serde_json::to_string_pretty(&doc)?)?;
            Some(p)
        };

        files.push(EmittedFile {
            csv_path,
            sentinel_path,
            books,
            rows: rows.len(),
            columns,
            as_of,
        });
    }

    Ok(EmittedDirectory { files, conflicting_instruments })
}

fn canonical_columns(omit: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for name in RiskBatch::IDENTITY {
        out.push((*name).to_string());
    }
    for group in [
        RiskBatch::UNDERLYING_MEASURES,
        RiskBatch::PAIR_MEASURES,
        RiskBatch::INSTRUMENT_MEASURES,
        RiskBatch::POSITION_MEASURES,
    ] {
        for name in group {
            if omit.contains(name) {
                continue;
            }
            out.push((*name).to_string());
            if USD_TWINS.contains(name) {
                out.push(format!("{name}__usd"));
            }
        }
    }
    out
}

fn header_columns(omit: &[&str]) -> Vec<String> {
    canonical_columns(omit)
        .iter()
        .map(|c| match c.strip_suffix("__usd") {
            Some(base) => format!("{}_USD", source_name(base)),
            None => source_name(c).to_string(),
        })
        .collect()
}

fn field_value(batch: &RiskBatch, i: usize, canonical: &str, plant_conflict: bool) -> String {
    if let Some(base) = canonical.strip_suffix("__usd") {
        // Deterministic FX factor, so the twin is reproducible.
        return format!("{:.6}", batch.measure(base)[i] * 1.08);
    }
    match canonical {
        "strike" => format!("{:.2}", batch.strike[i]),
        "model_code" if plant_conflict => "CONFLICT".to_string(),
        name if RiskBatch::IDENTITY.contains(&name) => batch.identity(name)[i].clone(),
        name => format!("{:.6}", batch.measure(name)[i]),
    }
}
```

- [ ] **Step 5: Run to verify tests pass**

Run: `cargo test -p geode-demo-data`
Expected: PASS (10 tests).

- [ ] **Step 6: Lint and commit**

Run: `cargo clippy -p geode-demo-data --all-targets -- -D warnings && cargo fmt --check`

```bash
git add crates/geode-demo-data
git commit -m "feat(demo-data): emit a realistic source directory

CSVs in the source's own column spelling with _USD twins, each paired
with a .done JSON sentinel carrying source time and expected columns.

Generates the awkward cases the ingest code must survive (spec §9.1): a
book split across two files, a file carrying two books, one CSV whose
sentinel has not landed, files missing optional greeks, staggered source
times so per-book freshness differs, and a planted instrument-attribute
disagreement for the conflict detector."
```

---

### Task 4: Sentinel parsing

**Files:**
- Create: `crates/geode-data/src/source/mod.rs`
- Create: `crates/geode-data/src/source/sentinel.rs`
- Modify: `crates/geode-data/src/lib.rs`
- Modify: `crates/geode-data/Cargo.toml`

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `Sentinel { as_of: OffsetDateTime-like String, columns: Vec<String>,
  books: Vec<String>, row_count: Option<usize>, dataset: Option<String> }`,
  `SentinelError`, and `parse_sentinel(&str) -> Result<Sentinel, SentinelError>`.
  Task 9 (load) and Task 10 (discovery) both consume this.

**Design constraint (spec §5.3):** only two fields are required — source time
and the column list. Everything unrecognised is ignored. The production shape
is an open question (spec §10.1), so the parser must not break when it
differs from the mock in any other respect.

- [ ] **Step 1: Add dependencies**

In `crates/geode-data/Cargo.toml`:

```toml
[dependencies]
geode-core.workspace = true
serde = { version = "1.0.228", features = ["derive"] }
serde_json = "1.0.151"
time = { version = "0.3.44", features = ["parsing", "formatting", "macros"] }

[dev-dependencies]
geode-demo-data = { path = "../geode-demo-data" }
tempfile = "3.27.0"
```

- [ ] **Step 2: Write the failing tests**

Create `crates/geode-data/src/source/sentinel.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"{
        "dataset": "risk_snapshot",
        "as_of": "2026-08-30T14:32:05Z",
        "business_date": "2026-08-30",
        "books": ["BK003", "BK011"],
        "row_count": 184203,
        "columns": ["Book", "LHU", "Delta01"]
    }"#;

    #[test]
    fn parses_the_documented_shape() {
        let s = parse_sentinel(FULL).unwrap();
        assert_eq!(s.columns, vec!["Book", "LHU", "Delta01"]);
        assert_eq!(s.books, vec!["BK003", "BK011"]);
        assert_eq!(s.row_count, Some(184_203));
        assert_eq!(s.dataset.as_deref(), Some("risk_snapshot"));
        assert_eq!(s.as_of.to_string(), "2026-08-30 14:32:05.0 +00:00:00");
    }

    #[test]
    fn requires_only_as_of_and_columns() {
        let s = parse_sentinel(r#"{"as_of":"2026-08-30T14:32:05Z","columns":["A"]}"#).unwrap();
        assert_eq!(s.columns, vec!["A"]);
        assert!(s.books.is_empty());
        assert_eq!(s.row_count, None);
    }

    #[test]
    fn ignores_unrecognised_fields() {
        let text = r#"{
            "as_of": "2026-08-30T14:32:05Z",
            "columns": ["A"],
            "producer": "riskrun",
            "nested": {"anything": [1, 2, 3]}
        }"#;
        assert!(parse_sentinel(text).is_ok());
    }

    #[test]
    fn missing_required_fields_name_what_is_missing() {
        let e = parse_sentinel(r#"{"columns":["A"]}"#).unwrap_err();
        assert!(e.to_string().contains("as_of"), "{e}");
        let e = parse_sentinel(r#"{"as_of":"2026-08-30T14:32:05Z"}"#).unwrap_err();
        assert!(e.to_string().contains("columns"), "{e}");
    }

    #[test]
    fn malformed_json_and_bad_timestamps_are_errors_not_panics() {
        assert!(parse_sentinel("not json").is_err());
        assert!(parse_sentinel(r#"{"as_of":"yesterday","columns":["A"]}"#).is_err());
    }

    #[test]
    fn accepts_an_offset_other_than_utc() {
        let s = parse_sentinel(r#"{"as_of":"2026-08-30T14:32:05+02:00","columns":["A"]}"#).unwrap();
        assert_eq!(s.as_of.offset().whole_hours(), 2);
    }
}
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test -p geode-data sentinel`
Expected: FAIL — `cannot find function parse_sentinel`.

- [ ] **Step 4: Implement the parser**

Prepend to `crates/geode-data/src/source/sentinel.rs`:

```rust
//! The `.done` sentinel (spec §5.3). Permissive by contract: only source
//! time and the expected column list are required, every unrecognised field
//! is ignored, and a missing required field is a health error naming the
//! file rather than a failure to ingest anything at all.
//!
//! The production shape is an open question (spec §10.1); this parser is
//! written so that only those two fields need to survive being wrong.

use serde::Deserialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sentinel {
    /// Authoritative source time: orders generations, drives as-of, and
    /// decides what is "most recent" (spec §4.4). Never file mtime.
    pub as_of: OffsetDateTime,
    /// Column spelling as it appears in the CSV header.
    pub columns: Vec<String>,
    pub books: Vec<String>,
    pub row_count: Option<usize>,
    pub dataset: Option<String>,
    pub business_date: Option<String>,
}

#[derive(Debug)]
pub enum SentinelError {
    Json(serde_json::Error),
    MissingField(&'static str),
    BadTimestamp { value: String, source: time::error::Parse },
}

impl std::fmt::Display for SentinelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SentinelError::Json(e) => write!(f, "sentinel is not valid JSON: {e}"),
            SentinelError::MissingField(name) => {
                write!(f, "sentinel is missing required field '{name}'")
            }
            SentinelError::BadTimestamp { value, source } => {
                write!(f, "sentinel 'as_of' value '{value}' is not RFC 3339: {source}")
            }
        }
    }
}

impl std::error::Error for SentinelError {}

/// Only the fields we understand; `serde` ignores the rest by default.
#[derive(Deserialize)]
struct Raw {
    as_of: Option<String>,
    columns: Option<Vec<String>>,
    #[serde(default)]
    books: Vec<String>,
    row_count: Option<usize>,
    dataset: Option<String>,
    business_date: Option<String>,
}

pub fn parse_sentinel(text: &str) -> Result<Sentinel, SentinelError> {
    let raw: Raw = serde_json::from_str(text).map_err(SentinelError::Json)?;
    let as_of_str = raw.as_of.ok_or(SentinelError::MissingField("as_of"))?;
    let as_of = OffsetDateTime::parse(&as_of_str, &Rfc3339)
        .map_err(|source| SentinelError::BadTimestamp { value: as_of_str, source })?;
    let columns = raw.columns.ok_or(SentinelError::MissingField("columns"))?;
    Ok(Sentinel {
        as_of,
        columns,
        books: raw.books,
        row_count: raw.row_count,
        dataset: raw.dataset,
        business_date: raw.business_date,
    })
}
```

- [ ] **Step 5: Wire the module up**

Create `crates/geode-data/src/source/mod.rs`:

```rust
//! Sources: configured origins of data (spec §5.1). A source is an adapter
//! plus a list of directory globs, a refresh interval, a readiness
//! strategy, a priority, and a column map.

pub mod sentinel;

pub use sentinel::{Sentinel, SentinelError, parse_sentinel};
```

Replace the body of `crates/geode-data/src/lib.rs` below its doc comment
with:

```rust
pub mod source;
```

- [ ] **Step 6: Run to verify tests pass**

Run: `cargo test -p geode-data`
Expected: PASS (6 tests).

- [ ] **Step 7: Commit**

```bash
git add crates/geode-data
git commit -m "feat(data): permissive .done sentinel parsing

Requires only source time and the expected column list, ignores every
unrecognised field, and names the missing field when a required one is
absent. The production sentinel shape is still an open question (spec
§10.1), so only those two fields need to survive being wrong.

Source time comes from the sentinel and never from file mtime, which any
copy or restore corrupts (spec §4.4)."
```

---

### Task 5: The store — persistent database and generated DDL

**Files:**
- Create: `crates/geode-data/src/store/mod.rs`
- Create: `crates/geode-data/src/store/ddl.rs`
- Modify: `crates/geode-data/src/lib.rs`
- Modify: `crates/geode-data/Cargo.toml`

**Interfaces:**
- Consumes: `geode_core::schema::{SchemaSpec, DatasetSpec, Grain, ColumnRole}`
  from Task 1.
- Produces: `Store::open(path) -> Result<Store, StoreError>`,
  `Store::writer() -> &Connection`, `Store::reader() -> Result<Connection, StoreError>`,
  `Store::apply_schema(&DatasetSpec) -> Result<(), StoreError>`, and
  `ddl::create_table_sql(&DatasetSpec, Grain) -> String`. Tasks 6–9 build on
  these.

**Verified against a real build:** `Connection::open` on a path gives a
persistent database; `try_clone()` yields a second connection on the same
database; the §4.3 publish transaction runs in ~3ms.

- [ ] **Step 1: Add the duckdb dependency**

In `crates/geode-data/Cargo.toml` `[dependencies]`:

```toml
duckdb = { version = "1.10505.0", features = ["bundled"] }
```

Note for the implementer: the first build compiles DuckDB from C++ source
and takes minutes (measured 127s wall / 1382s CPU on an M-series Mac). It is
cached thereafter.

- [ ] **Step 2: Write the failing DDL tests**

Create `crates/geode-data/src/store/ddl.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::schema::SchemaSpec;

    fn dataset() -> geode_core::schema::DatasetSpec {
        let text = r#"
[risk_snapshot.columns.book]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.lhu]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.position_ref]
type = "utf8"
role = "key"
[risk_snapshot.columns.counterparty]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.instrument_ref]
type = "utf8"
role = "key"
[risk_snapshot.columns.underlying_ref]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.delta01]
type = "f64"
role = "measure"
grain = "underlying"
[risk_snapshot.columns.daily_trading_pnl]
type = "f64"
role = "measure"
grain = "position"
"#;
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        SchemaSpec::from_doc(&doc).0.dataset("risk_snapshot").unwrap().clone()
    }

    #[test]
    fn live_table_carries_the_grain_key_and_its_measures_only() {
        let sql = create_table_sql(&dataset(), Grain::Position, TableKind::Live);
        assert!(sql.contains("measures_position_live"), "{sql}");
        assert!(sql.contains("\"book\" VARCHAR"), "{sql}");
        assert!(sql.contains("\"daily_trading_pnl\" DOUBLE"), "{sql}");
        // A finer grain's key column must not appear at position grain.
        assert!(!sql.contains("underlying_ref"), "{sql}");
        // Nor a measure declared at another grain.
        assert!(!sql.contains("delta01"), "{sql}");
        // Live carries no generation column (spec §4.2).
        assert!(!sql.contains("gen_id"), "{sql}");
    }

    #[test]
    fn live_carries_source_file_id_as_the_replacement_key() {
        let sql = create_table_sql(&dataset(), Grain::Underlying, TableKind::Live);
        assert!(sql.contains("\"source_file_id\" BIGINT"), "{sql}");
    }

    #[test]
    fn archive_adds_gen_id_and_source_time() {
        let sql = create_table_sql(&dataset(), Grain::Underlying, TableKind::Archive);
        assert!(sql.contains("measures_underlying_archive"), "{sql}");
        assert!(sql.contains("\"gen_id\" BIGINT"), "{sql}");
        assert!(sql.contains("\"source_time\" TIMESTAMP WITH TIME ZONE"), "{sql}");
    }
}
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test -p geode-data ddl`
Expected: FAIL — `cannot find function create_table_sql`.

- [ ] **Step 4: Implement DDL generation**

Prepend to `crates/geode-data/src/store/ddl.rs`:

```rust
//! Table DDL generated from the declared schema (spec §4.2). One live and
//! one archive table per grain present in the dataset.
//!
//! Live carries exactly the current rows for every file partition: no
//! generation column, no history predicate, size independent of retention.
//! That is what keeps the requery budget reachable by construction, so the
//! absence of `gen_id` from live is load-bearing, not an oversight.

use geode_core::schema::{ColumnRole, DatasetSpec, Grain};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableKind {
    Live,
    Archive,
}

impl TableKind {
    pub fn suffix(self) -> &'static str {
        match self {
            TableKind::Live => "_live",
            TableKind::Archive => "_archive",
        }
    }
}

pub fn table_name(grain: Grain, kind: TableKind) -> String {
    format!("{}{}", grain.table(), kind.suffix())
}

pub fn create_table_sql(ds: &DatasetSpec, grain: Grain, kind: TableKind) -> String {
    let mut cols: Vec<String> = Vec::new();

    for key in grain.key_columns() {
        let ty = ds
            .column(key)
            .map(|c| c.ty.sql())
            .unwrap_or("VARCHAR");
        cols.push(format!("  \"{key}\" {ty}"));
    }

    // Pair grain carries no extra dimensions beyond its key; other grains
    // carry the dimensions declared for them.
    for c in ds.columns.iter() {
        let keep = match c.role {
            ColumnRole::Measure { grain: g, .. } | ColumnRole::Attribute { grain: g } => g == grain,
            ColumnRole::Key | ColumnRole::Dimension => false,
        };
        if keep {
            cols.push(format!("  \"{}\" {}", c.name, c.ty.sql()));
        }
    }

    cols.push("  \"source_file_id\" BIGINT".to_string());
    if kind == TableKind::Archive {
        cols.push("  \"gen_id\" BIGINT".to_string());
        cols.push("  \"source_time\" TIMESTAMP WITH TIME ZONE".to_string());
    }

    format!(
        "CREATE TABLE IF NOT EXISTS {} (\n{}\n);",
        table_name(grain, kind),
        cols.join(",\n")
    )
}
```

- [ ] **Step 5: Run to verify DDL tests pass**

Run: `cargo test -p geode-data ddl`
Expected: PASS (3 tests).

- [ ] **Step 6: Write the failing store tests**

Create `crates/geode-data/src/store/mod.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_a_persistent_database_that_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("geode.duckdb");
        {
            let store = Store::open(&path).unwrap();
            store
                .writer()
                .execute_batch("create table probe(x integer); insert into probe values (7);")
                .unwrap();
        }
        assert!(path.exists(), "database file must be on disk");
        let store = Store::open(&path).unwrap();
        let x: i32 = store
            .writer()
            .query_row("select x from probe", [], |r| r.get(0))
            .unwrap();
        assert_eq!(x, 7, "data must survive reopen (spec §2.1)");
    }

    #[test]
    fn readers_are_independent_connections_on_the_same_database() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        store
            .writer()
            .execute_batch("create table probe(x integer); insert into probe values (1),(2);")
            .unwrap();
        let reader = store.reader().unwrap();
        let n: i64 = reader.query_row("select count(*) from probe", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn apply_schema_creates_live_and_archive_per_declared_grain() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        store.apply_schema(&super::ddl::tests_support::sample_dataset()).unwrap();

        let tables: Vec<String> = {
            let conn = store.writer();
            let mut stmt = conn
                .prepare("select table_name from information_schema.tables order by table_name")
                .unwrap();
            let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
            rows.map(|r| r.unwrap()).collect()
        };
        for expected in [
            "measures_position_live",
            "measures_position_archive",
            "measures_underlying_live",
            "measures_underlying_archive",
        ] {
            assert!(tables.contains(&expected.to_string()), "missing {expected}: {tables:?}");
        }
    }

    #[test]
    fn apply_schema_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        let ds = super::ddl::tests_support::sample_dataset();
        store.apply_schema(&ds).unwrap();
        store.apply_schema(&ds).unwrap();
    }
}
```

In `crates/geode-data/src/store/ddl.rs`, add a shared fixture the store
tests can reuse (this replaces the private `dataset()` helper — move its
body here and have the ddl tests call it):

```rust
#[cfg(test)]
pub(crate) mod tests_support {
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::schema::{DatasetSpec, SchemaSpec};

    pub(crate) fn sample_dataset() -> DatasetSpec {
        let text = r#"
[risk_snapshot.columns.book]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.lhu]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.position_ref]
type = "utf8"
role = "key"
[risk_snapshot.columns.counterparty]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.instrument_ref]
type = "utf8"
role = "key"
[risk_snapshot.columns.underlying_ref]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.delta01]
type = "f64"
role = "measure"
grain = "underlying"
[risk_snapshot.columns.daily_trading_pnl]
type = "f64"
role = "measure"
grain = "position"
"#;
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        SchemaSpec::from_doc(&doc).0.dataset("risk_snapshot").unwrap().clone()
    }
}
```

- [ ] **Step 7: Run to verify it fails**

Run: `cargo test -p geode-data store`
Expected: FAIL — `cannot find struct Store`.

- [ ] **Step 8: Implement the store**

Prepend to `crates/geode-data/src/store/mod.rs`:

```rust
//! The store: one persistent DuckDB database that is the system of record
//! (spec §4.1). History survives relaunch and CSV ingest is paid once.
//!
//! One dedicated writer connection serves ingest; readers are independent
//! connections on the same database (spec §5.3). No in-memory mirror: a
//! dual store doubles the coherency surface for a win nothing has measured.

pub mod ddl;

use duckdb::Connection;
use geode_core::schema::DatasetSpec;
use std::path::{Path, PathBuf};

use ddl::TableKind;

#[derive(Debug)]
pub enum StoreError {
    Open { path: PathBuf, source: duckdb::Error },
    Sql { statement: String, source: duckdb::Error },
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Open { path, source } => {
                write!(f, "opening database at {}: {source}", path.display())
            }
            StoreError::Sql { statement, source } => {
                write!(f, "executing `{statement}`: {source}")
            }
        }
    }
}

impl std::error::Error for StoreError {}

pub struct Store {
    writer: Connection,
    path: PathBuf,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Store, StoreError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let writer = Connection::open(&path)
            .map_err(|source| StoreError::Open { path: path.clone(), source })?;
        Ok(Store { writer, path })
    }

    /// The single writer connection. DuckDB is single-writer/multi-reader,
    /// so every publish transaction serializes through this (spec §5.6).
    pub fn writer(&self) -> &Connection {
        &self.writer
    }

    /// A fresh read connection on the same database, for the query pool.
    pub fn reader(&self) -> Result<Connection, StoreError> {
        self.writer
            .try_clone()
            .map_err(|source| StoreError::Open { path: self.path.clone(), source })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Create the live and archive tables for every grain the dataset
    /// declares measures at. Idempotent.
    pub fn apply_schema(&self, ds: &DatasetSpec) -> Result<(), StoreError> {
        for grain in ds.grains() {
            for kind in [TableKind::Live, TableKind::Archive] {
                let sql = ddl::create_table_sql(ds, grain, kind);
                self.writer
                    .execute_batch(&sql)
                    .map_err(|source| StoreError::Sql { statement: sql, source })?;
            }
        }
        Ok(())
    }
}
```

Add to `crates/geode-data/src/lib.rs`:

```rust
pub mod store;
```

- [ ] **Step 9: Run the full data suite**

Run: `cargo test -p geode-data && cargo clippy -p geode-data --all-targets -- -D warnings`
Expected: PASS (13 tests), no warnings.

- [ ] **Step 10: Commit**

```bash
git add crates/geode-data
git commit -m "feat(data): persistent store with schema-generated DDL

One DuckDB file as the system of record (spec §4.1), a dedicated writer
connection, and independent reader connections via try_clone. Live and
archive tables are generated per declared grain: live carries the grain
key, its own measures and source_file_id but no generation column, which
is what keeps its size independent of retention (spec §4.2)."
```

---

**Remaining tasks (6–14) continue below.** Task 6 adds the `file_generations`
catalog and freshness rollup; Task 7 the publish transaction and backfill
guard; Task 8 the grain split and conflict detection; Task 9 the per-file
load pipeline. Tasks 10–12 add discovery, the priority ladder, and the
ingest runner with its panic boundary. Tasks 13–14 add the retention sweeper
and the ingest benchmarks.
