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

**Remaining tasks (3–14) continue in this document.** Task 3 emits the
source directory (CSVs plus JSON sentinels, including a book split across
two files, a file carrying two books, a CSV with no sentinel yet, and
deliberate instrument-attribute disagreements). Tasks 4–9 build sentinel
parsing, the store, the catalog, the publish transaction, the grain split,
and the load pipeline. Tasks 10–12 build discovery, the priority ladder,
and the ingest runner. Tasks 13–14 add the retention sweeper and the ingest
benchmarks.
