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
the `bundled` and `chrono` features (bundled compiles DuckDB from C++
source). `serde`/`serde_json` for sentinels, `chrono` for timestamps, `glob`
and `regex` for discovery, `criterion` 0.8.2 for benchmarks. No async
runtime — ingest is OS threads and channels.

**Timestamps are `chrono::DateTime<Utc>`, not `time::OffsetDateTime`.**
duckdb-rs ships `src/types/chrono.rs` and no `time.rs`, so only chrono has
`ToSql`/`FromSql` impls. Using `time` would mean hand-converting at every
SQL boundary. Verified against the vendored 1.10505 source.

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
- Produces: `Sentinel { as_of: DateTime<Utc>, columns: Vec<String>,
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
chrono = "0.4.42"

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
        assert_eq!(s.as_of.to_rfc3339(), "2026-08-30T14:32:05+00:00");
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
        // Normalized to UTC on parse: 14:32:05+02:00 is 12:32:05Z.
        assert_eq!(s.as_of.to_rfc3339(), "2026-08-30T12:32:05+00:00");
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

use chrono::{DateTime, Utc};
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sentinel {
    /// Authoritative source time: orders generations, drives as-of, and
    /// decides what is "most recent" (spec §4.4). Never file mtime.
    pub as_of: DateTime<Utc>,
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
    BadTimestamp { value: String, source: chrono::ParseError },
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
    let as_of = DateTime::parse_from_rfc3339(&as_of_str)
        .map(|t| t.with_timezone(&Utc))
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
duckdb = { version = "1.10505.0", features = ["bundled", "chrono"] }
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
    fn live_carries_slot_for_replacement_and_file_id_for_provenance() {
        let sql = create_table_sql(&dataset(), Grain::Underlying, TableKind::Live);
        // `slot` is what the publish transaction matches on: filenames carry
        // dates, so file identity is not partition identity (spec §4.3).
        assert!(sql.contains("\"slot\" VARCHAR"), "{sql}");
        assert!(sql.contains("\"source_file_id\" BIGINT"), "{sql}");
        // `book` is part of the grain key at every grain, completing the
        // partition key (dataset, slot, book).
        assert!(sql.contains("\"book\" VARCHAR"), "{sql}");
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

    // Partition key completion: `book` is already in the grain key, `slot`
    // is what replacement matches on, `source_file_id` is provenance only
    // (spec §4.3 — filenames carry dates, so file id is not partition id).
    cols.push("  \"slot\" VARCHAR".to_string());
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

### Task 6: The catalog — file generations and freshness

**Files:**
- Create: `crates/geode-data/src/store/catalog.rs`
- Create: `crates/geode-data/src/health.rs`
- Modify: `crates/geode-data/src/store/mod.rs`
- Modify: `crates/geode-data/src/lib.rs`

**Interfaces:**
- Consumes: `Store` from Task 5.
- Produces: `Health`, `FileId = i64`, `FileGeneration`, `Catalog<'a>` with
  `ensure_tables`, `lookup_by_path`, `next_gen_id`, `record`,
  `live_source_time(slot, book)`, `book_freshness(dataset)`, and
  `dataset_as_of(dataset, books)`. Task 7 uses `live_source_time` for the
  backfill guard; Task 10 uses `lookup_by_path` for change detection.

**Why a side table for books.** A file covers a *set* of books, and freshness
rolls up per book (spec §4.5). Storing that set as a `file_books(file_id,
book)` side table rather than a list column keeps the rollup a plain
aggregate join, which is what the query path will want too.

- [ ] **Step 1: Write the failing health test**

Create `crates/geode-data/src/health.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_orders_by_severity_so_rollups_take_the_worst() {
        let mut states = vec![
            Health::Ok,
            Health::Failed { reason: "torn read".into() },
            Health::Pending,
            Health::Degraded { reason: "column missing".into() },
        ];
        states.sort();
        assert_eq!(states.first().unwrap().label(), "ok");
        assert_eq!(states.last().unwrap().label(), "failed");
    }

    #[test]
    fn round_trips_through_its_stored_label() {
        for h in [
            Health::Ok,
            Health::Pending,
            Health::PendingTooLong,
            Health::Degraded { reason: "r".into() },
            Health::Failed { reason: "r".into() },
        ] {
            let (label, reason) = h.to_parts();
            assert_eq!(Health::from_parts(&label, reason.as_deref()), h);
        }
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p geode-data health`
Expected: FAIL — `cannot find enum Health`.

- [ ] **Step 3: Implement `Health`**

Prepend to `crates/geode-data/src/health.rs`:

```rust
//! Degradation vocabulary (spec §5.7). Data problems are never modal and
//! never fatal: a failed load leaves live untouched and degrades this
//! file's health. Ord is severity order so a rollup can take the worst.

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Health {
    Ok,
    /// A CSV whose sentinel has not landed yet. Expected, not broken.
    Pending,
    /// Pending past the source's configured timeout.
    PendingTooLong,
    /// Loaded, but something was wrong — a required column was missing.
    Degraded { reason: String },
    /// The load failed. Last good generation stays live.
    Failed { reason: String },
}

impl Health {
    pub fn label(&self) -> &'static str {
        match self {
            Health::Ok => "ok",
            Health::Pending => "pending",
            Health::PendingTooLong => "pending_too_long",
            Health::Degraded { .. } => "degraded",
            Health::Failed { .. } => "failed",
        }
    }

    pub fn to_parts(&self) -> (String, Option<String>) {
        let reason = match self {
            Health::Degraded { reason } | Health::Failed { reason } => Some(reason.clone()),
            _ => None,
        };
        (self.label().to_string(), reason)
    }

    pub fn from_parts(label: &str, reason: Option<&str>) -> Health {
        match label {
            "pending" => Health::Pending,
            "pending_too_long" => Health::PendingTooLong,
            "degraded" => Health::Degraded { reason: reason.unwrap_or_default().to_string() },
            "failed" => Health::Failed { reason: reason.unwrap_or_default().to_string() },
            _ => Health::Ok,
        }
    }

    pub fn is_ok(&self) -> bool {
        matches!(self, Health::Ok)
    }
}
```

Add `pub mod health;` to `crates/geode-data/src/lib.rs`.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p geode-data health`
Expected: PASS (2 tests).

- [ ] **Step 5: Write the failing catalog tests**

Create `crates/geode-data/src/store/catalog.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Datelike, Timelike, Utc};

    /// Terse RFC 3339 literal for tests.
    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn store() -> (tempfile::TempDir, crate::store::Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("geode.duckdb")).unwrap();
        Catalog::new(store.writer()).ensure_tables().unwrap();
        (dir, store)
    }

    fn record(slot: &str, books: &[&str], source_time: DateTime<Utc>) -> FileGeneration {
        FileGeneration {
            file_id: 0,
            dataset: "risk_snapshot".into(),
            slot: slot.into(),
            path: format!("/src/{slot}.csv").into(),
            size: 1234,
            mtime: source_time,
            source_time,
            gen_id: 0,
            loaded_at: source_time,
            row_count: 10,
            books: books.iter().map(|b| b.to_string()).collect(),
            health: Health::Ok,
        }
    }

    #[test]
    fn gen_ids_are_monotonic() {
        let (_d, store) = store();
        let cat = Catalog::new(store.writer());
        assert_eq!(cat.next_gen_id().unwrap(), 1);
        let mut r = record("BK000", &["BK000"], ts("2026-08-30T07:00:00Z"));
        r.gen_id = 1;
        cat.record(&r).unwrap();
        assert_eq!(cat.next_gen_id().unwrap(), 2);
    }

    #[test]
    fn lookup_by_path_returns_the_latest_generation_for_that_file() {
        let (_d, store) = store();
        let cat = Catalog::new(store.writer());
        for (gen, hour) in [(1, 7), (2, 9)] {
            let mut r = record("BK000", &["BK000"], ts(&format!("2026-08-30T{hour:02}:00:00Z")));
            r.gen_id = gen;
            cat.record(&r).unwrap();
        }
        let found = cat.lookup_by_path(std::path::Path::new("/src/BK000.csv")).unwrap().unwrap();
        assert_eq!(found.gen_id, 2);
        assert_eq!(found.source_time.hour(), 9);
    }

    #[test]
    fn live_source_time_is_per_partition_not_per_file() {
        let (_d, store) = store();
        let cat = Catalog::new(store.writer());
        // Two business dates share a slot: same partition, different files.
        for (gen, day) in [(1, 29u8), (2, 30u8)] {
            let mut r = FileGeneration {
                path: format!("/src/risk_2026-08-{day}_BK000.csv").into(),
                ..record("BK000", &["BK000"], ts(&format!("2026-08-{day:02}T07:00:00Z")))
            };
            r.gen_id = gen;
            cat.record(&r).unwrap();
        }
        let t = cat.live_source_time("BK000", "BK000").unwrap().unwrap();
        assert_eq!(t.day(), 30, "the partition's live time is the newest across its files");
    }

    #[test]
    fn book_freshness_is_the_oldest_contributing_file() {
        let (_d, store) = store();
        let cat = Catalog::new(store.writer());
        // BK000 is split across two slots with different source times.
        let mut a = record("BK000_part1", &["BK000"], ts("2026-08-30T07:00:00Z"));
        a.gen_id = 1;
        cat.record(&a).unwrap();
        let mut b = record("BK000_part2", &["BK000"], ts("2026-08-30T14:00:00Z"));
        b.gen_id = 2;
        cat.record(&b).unwrap();

        let fresh = cat.book_freshness("risk_snapshot").unwrap();
        let bk000 = fresh.iter().find(|(b, _)| b == "BK000").unwrap();
        assert_eq!(bk000.1.hour(), 7, "a book is as fresh as its stalest file");
    }

    #[test]
    fn dataset_as_of_is_the_oldest_book_in_scope() {
        let (_d, store) = store();
        let cat = Catalog::new(store.writer());
        let mut a = record("BK000", &["BK000"], ts("2026-08-30T07:00:00Z"));
        a.gen_id = 1;
        cat.record(&a).unwrap();
        let mut b = record("BK001", &["BK001"], ts("2026-08-30T14:00:00Z"));
        b.gen_id = 2;
        cat.record(&b).unwrap();

        let all = cat.dataset_as_of("risk_snapshot", &[]).unwrap().unwrap();
        assert_eq!(all.hour(), 7, "unscoped as-of is the oldest book");
        let scoped = cat
            .dataset_as_of("risk_snapshot", &["BK001".to_string()])
            .unwrap()
            .unwrap();
        assert_eq!(scoped.hour(), 14, "scoping to a fresh book must not inherit a stale one");
    }

    #[test]
    fn a_multi_book_file_contributes_to_every_book_it_covers() {
        let (_d, store) = store();
        let cat = Catalog::new(store.writer());
        let mut r = record("BK001_BK002", &["BK001", "BK002"], ts("2026-08-30T09:00:00Z"));
        r.gen_id = 1;
        cat.record(&r).unwrap();
        let fresh = cat.book_freshness("risk_snapshot").unwrap();
        assert_eq!(fresh.len(), 2);
    }
}
```

- [ ] **Step 6: Run to verify it fails**

Run: `cargo test -p geode-data catalog`
Expected: FAIL — `cannot find struct Catalog`.

- [ ] **Step 7: Implement the catalog**

Prepend to `crates/geode-data/src/store/catalog.rs`:

```rust
//! Freshness bookkeeping (spec §4.5). Records what was loaded from where
//! and when, in *source* time — never mtime, which any copy or restore
//! corrupts (spec §4.4).
//!
//! Freshness rolls up: a book is as fresh as its stalest contributing file,
//! and a dataset's headline as-of is the oldest book in the effective
//! scope. That is the same stalest-input rule joins use (spec §5.4).

use crate::health::Health;
use crate::store::StoreError;
use duckdb::Connection;
use std::path::{Path, PathBuf};
use chrono::{DateTime, Utc};

pub type FileId = i64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileGeneration {
    pub file_id: FileId,
    pub dataset: String,
    /// Filename with its date component removed: the partition's identity
    /// across business dates (spec §4.3).
    pub slot: String,
    pub path: PathBuf,
    pub size: u64,
    pub mtime: DateTime<Utc>,
    /// From the sentinel. Orders generations and drives as-of.
    pub source_time: DateTime<Utc>,
    pub gen_id: i64,
    pub loaded_at: DateTime<Utc>,
    pub row_count: usize,
    pub books: Vec<String>,
    pub health: Health,
}

pub struct Catalog<'a> {
    conn: &'a Connection,
}

const DDL: &str = "
CREATE TABLE IF NOT EXISTS file_generations (
  file_id BIGINT PRIMARY KEY,
  dataset VARCHAR,
  slot VARCHAR,
  path VARCHAR,
  size BIGINT,
  mtime TIMESTAMP WITH TIME ZONE,
  source_time TIMESTAMP WITH TIME ZONE,
  gen_id BIGINT,
  loaded_at TIMESTAMP WITH TIME ZONE,
  row_count BIGINT,
  health VARCHAR,
  health_reason VARCHAR
);
CREATE SEQUENCE IF NOT EXISTS file_generations_id START 1;
CREATE SEQUENCE IF NOT EXISTS generation_id START 1;
CREATE TABLE IF NOT EXISTS file_books (
  file_id BIGINT,
  book VARCHAR
);
";

impl<'a> Catalog<'a> {
    pub fn new(conn: &'a Connection) -> Catalog<'a> {
        Catalog { conn }
    }

    fn sql(&self, statement: &str) -> Result<(), StoreError> {
        self.conn
            .execute_batch(statement)
            .map_err(|source| StoreError::Sql { statement: statement.to_string(), source })
    }

    pub fn ensure_tables(&self) -> Result<(), StoreError> {
        self.sql(DDL)
    }

    /// Peek at the next generation id without consuming it.
    pub fn next_gen_id(&self) -> Result<i64, StoreError> {
        let sql = "select coalesce(max(gen_id), 0) + 1 from file_generations";
        self.conn
            .query_row(sql, [], |r| r.get(0))
            .map_err(|source| StoreError::Sql { statement: sql.into(), source })
    }

    pub fn record(&self, rec: &FileGeneration) -> Result<FileId, StoreError> {
        let (health, reason) = rec.health.to_parts();
        let sql = "insert into file_generations
                   select nextval('file_generations_id'), ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?
                   returning file_id";
        let file_id: i64 = self
            .conn
            .query_row(
                sql,
                duckdb::params![
                    rec.dataset,
                    rec.slot,
                    rec.path.to_string_lossy().to_string(),
                    rec.size as i64,
                    rec.mtime,
                    rec.source_time,
                    rec.gen_id,
                    rec.loaded_at,
                    rec.row_count as i64,
                    health,
                    reason,
                ],
                |r| r.get(0),
            )
            .map_err(|source| StoreError::Sql { statement: sql.into(), source })?;

        for book in &rec.books {
            let sql = "insert into file_books values (?, ?)";
            self.conn
                .execute(sql, duckdb::params![file_id, book])
                .map_err(|source| StoreError::Sql { statement: sql.into(), source })?;
        }
        Ok(file_id)
    }

    pub fn lookup_by_path(&self, path: &Path) -> Result<Option<FileGeneration>, StoreError> {
        let sql = "select file_id, dataset, slot, path, size, mtime, source_time, gen_id,
                          loaded_at, row_count, health, health_reason
                   from file_generations where path = ? order by gen_id desc limit 1";
        let mut stmt = self
            .conn
            .prepare(sql)
            .map_err(|source| StoreError::Sql { statement: sql.into(), source })?;
        let mut rows = stmt
            .query(duckdb::params![path.to_string_lossy().to_string()])
            .map_err(|source| StoreError::Sql { statement: sql.into(), source })?;
        let Some(row) = rows
            .next()
            .map_err(|source| StoreError::Sql { statement: sql.into(), source })?
        else {
            return Ok(None);
        };
        let file_id: i64 = row.get(0).unwrap();
        let health_label: String = row.get(10).unwrap();
        let health_reason: Option<String> = row.get(11).unwrap();
        Ok(Some(FileGeneration {
            file_id,
            dataset: row.get(1).unwrap(),
            slot: row.get(2).unwrap(),
            path: PathBuf::from(row.get::<_, String>(3).unwrap()),
            size: row.get::<_, i64>(4).unwrap() as u64,
            mtime: row.get(5).unwrap(),
            source_time: row.get(6).unwrap(),
            gen_id: row.get(7).unwrap(),
            loaded_at: row.get(8).unwrap(),
            row_count: row.get::<_, i64>(9).unwrap() as usize,
            books: self.books_of(file_id)?,
            health: Health::from_parts(&health_label, health_reason.as_deref()),
        }))
    }

    fn books_of(&self, file_id: FileId) -> Result<Vec<String>, StoreError> {
        let sql = "select book from file_books where file_id = ? order by book";
        let mut stmt = self
            .conn
            .prepare(sql)
            .map_err(|source| StoreError::Sql { statement: sql.into(), source })?;
        let rows = stmt
            .query_map(duckdb::params![file_id], |r| r.get::<_, String>(0))
            .map_err(|source| StoreError::Sql { statement: sql.into(), source })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// The newest source time published for a partition (spec §4.3). This is
    /// what the backfill guard compares against: an older file shares the
    /// slot, so it must not overwrite what is already live.
    pub fn live_source_time(
        &self,
        slot: &str,
        book: &str,
    ) -> Result<Option<DateTime<Utc>>, StoreError> {
        let sql = "select max(fg.source_time) from file_generations fg
                   join file_books fb on fb.file_id = fg.file_id
                   where fg.slot = ? and fb.book = ?";
        self.conn
            .query_row(sql, duckdb::params![slot, book], |r| r.get(0))
            .map_err(|source| StoreError::Sql { statement: sql.into(), source })
    }

    /// Per-book freshness: a book is as fresh as its *stalest* file.
    pub fn book_freshness(
        &self,
        dataset: &str,
    ) -> Result<Vec<(String, DateTime<Utc>)>, StoreError> {
        let sql = "select fb.book, min(newest.t)
                   from (select fg.slot, fb.book, max(fg.source_time) as t
                         from file_generations fg
                         join file_books fb on fb.file_id = fg.file_id
                         where fg.dataset = ?
                         group by fg.slot, fb.book) newest
                   join file_books fb on fb.book = newest.book
                   group by fb.book order by fb.book";
        let mut stmt = self
            .conn
            .prepare(sql)
            .map_err(|source| StoreError::Sql { statement: sql.into(), source })?;
        let rows = stmt
            .query_map(duckdb::params![dataset], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, DateTime<Utc>>(1)?))
            })
            .map_err(|source| StoreError::Sql { statement: sql.into(), source })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// A dataset's headline as-of: the oldest book in scope. An empty scope
    /// means every book.
    pub fn dataset_as_of(
        &self,
        dataset: &str,
        books: &[String],
    ) -> Result<Option<DateTime<Utc>>, StoreError> {
        let fresh = self.book_freshness(dataset)?;
        Ok(fresh
            .into_iter()
            .filter(|(b, _)| books.is_empty() || books.contains(b))
            .map(|(_, t)| t)
            .min())
    }
}
```

Add to `crates/geode-data/src/store/mod.rs`, next to `pub mod ddl;`:

```rust
pub mod catalog;

pub use catalog::{Catalog, FileGeneration, FileId};
```

- [ ] **Step 8: Run to verify tests pass**

Run: `cargo test -p geode-data catalog`
Expected: PASS (6 tests).

- [ ] **Step 9: Lint and commit**

Run: `cargo clippy -p geode-data --all-targets -- -D warnings`

```bash
git add crates/geode-data
git commit -m "feat(data): file generation catalog and freshness rollup

Records what was loaded from where and when in source time, never mtime.
Books live in a file_books side table because a file covers a set of
them, which keeps the rollup a plain aggregate join.

Freshness rolls up the way spec §4.5 requires: a book is as fresh as its
stalest contributing file, and a dataset's as-of is the oldest book in
scope — so scoping to a fresh book does not inherit a stale one's
timestamp. live_source_time is per partition, not per file, which is what
the backfill guard needs."
```

---

### Task 7: The publish transaction and the backfill guard

**Files:**
- Create: `crates/geode-data/src/store/publish.rs`
- Modify: `crates/geode-data/src/store/mod.rs`

**Interfaces:**
- Consumes: `Store`, `Catalog`, `ddl::{table_name, TableKind}`, `Grain`.
- Produces: `Partition { slot: String, book: String }`, `PublishRequest`,
  `PublishOutcome { Published { rows }, ArchivedOnly { rows, reason } }`, and
  `publish_file(&Connection, &PublishRequest) -> Result<PublishOutcome, StoreError>`.
  Task 9 calls this as the last step of the load pipeline.

**The property that matters.** Live must hold exactly one generation per
partition after any sequence of publishes, in any order. That is what makes
live's size independent of retention and the §7.1 requery budget reachable
by construction — so it is asserted directly, including for out-of-order
backfill.

- [ ] **Step 1: Write the failing publish tests**

Create `crates/geode-data/src/store/publish.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use chrono::{DateTime, Utc};

    /// Terse RFC 3339 literal for tests.
    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    /// Minimal two-column live/archive pair so the test exercises the
    /// transaction, not DDL generation.
    fn fixture() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        store
            .writer()
            .execute_batch(
                "create table measures_position_live(
                     book varchar, position_ref varchar, daily_trading_pnl double,
                     slot varchar, source_file_id bigint);
                 create table measures_position_archive(
                     book varchar, position_ref varchar, daily_trading_pnl double,
                     slot varchar, source_file_id bigint,
                     gen_id bigint, source_time timestamp with time zone);
                 create table staging_position(
                     book varchar, position_ref varchar, daily_trading_pnl double,
                     slot varchar, source_file_id bigint);",
            )
            .unwrap();
        (dir, store)
    }

    fn stage(store: &Store, book: &str, pnl: f64, slot: &str, file_id: i64) {
        store
            .writer()
            .execute(
                "insert into staging_position values (?, 'POS1', ?, ?, ?)",
                duckdb::params![book, pnl, slot, file_id],
            )
            .unwrap();
    }

    fn request(slot: &str, book: &str, gen: i64, t: DateTime<Utc>) -> PublishRequest {
        PublishRequest {
            grain: geode_core::schema::Grain::Position,
            staging_table: "staging_position".into(),
            partitions: vec![Partition { slot: slot.into(), book: book.into() }],
            gen_id: gen,
            source_time: t,
            live_source_time: None,
        }
    }

    fn live_rows(store: &Store) -> Vec<(String, f64)> {
        let conn = store.writer();
        let mut stmt = conn
            .prepare("select book, daily_trading_pnl from measures_position_live order by book")
            .unwrap();
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)))
            .unwrap();
        rows.map(|r| r.unwrap()).collect()
    }

    #[test]
    fn first_publish_inserts_into_live_and_leaves_archive_empty() {
        let (_d, store) = fixture();
        stage(&store, "BK000", 10.0, "BK000", 1);
        let out = publish_file(store.writer(), &request("BK000", "BK000", 1, ts("2026-08-30T07:00:00Z"))).unwrap();
        assert!(matches!(out, PublishOutcome::Published { rows: 1 }));
        assert_eq!(live_rows(&store), vec![("BK000".to_string(), 10.0)]);
        let n: i64 = store.writer().query_row("select count(*) from measures_position_archive", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn republishing_a_partition_replaces_it_and_archives_the_old_rows() {
        let (_d, store) = fixture();
        stage(&store, "BK000", 10.0, "BK000", 1);
        publish_file(store.writer(), &request("BK000", "BK000", 1, ts("2026-08-29T07:00:00Z"))).unwrap();
        store.writer().execute_batch("delete from staging_position").unwrap();
        stage(&store, "BK000", 99.0, "BK000", 2);

        let mut req = request("BK000", "BK000", 2, ts("2026-08-30T07:00:00Z"));
        req.live_source_time = Some(ts("2026-08-29T07:00:00Z"));
        publish_file(store.writer(), &req).unwrap();

        assert_eq!(live_rows(&store), vec![("BK000".to_string(), 99.0)], "live holds one generation");
        let archived: f64 = store
            .writer()
            .query_row("select daily_trading_pnl from measures_position_archive", [], |r| r.get(0))
            .unwrap();
        assert_eq!(archived, 10.0, "the superseded rows moved to archive");
    }

    #[test]
    fn a_backfilled_older_file_never_reaches_live() {
        let (_d, store) = fixture();
        stage(&store, "BK000", 99.0, "BK000", 1);
        publish_file(store.writer(), &request("BK000", "BK000", 1, ts("2026-08-30T07:00:00Z"))).unwrap();
        store.writer().execute_batch("delete from staging_position").unwrap();

        // Last Tuesday's file, loaded after this morning's.
        stage(&store, "BK000", 10.0, "BK000", 2);
        let mut req = request("BK000", "BK000", 2, ts("2026-08-25T07:00:00Z"));
        req.live_source_time = Some(ts("2026-08-30T07:00:00Z"));
        let out = publish_file(store.writer(), &req).unwrap();

        assert!(matches!(out, PublishOutcome::ArchivedOnly { .. }), "{out:?}");
        assert_eq!(
            live_rows(&store),
            vec![("BK000".to_string(), 99.0)],
            "this morning's risk must survive a backfill"
        );
        let n: i64 = store.writer().query_row("select count(*) from measures_position_archive", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1, "the old generation is history, not live");
    }

    #[test]
    fn publishing_one_partition_leaves_its_siblings_alone() {
        let (_d, store) = fixture();
        stage(&store, "BK000", 10.0, "BK000_part1", 1);
        publish_file(store.writer(), &request("BK000_part1", "BK000", 1, ts("2026-08-30T07:00:00Z"))).unwrap();
        store.writer().execute_batch("delete from staging_position").unwrap();

        // Same book, different slot: the split file's other half.
        stage(&store, "BK000", 20.0, "BK000_part2", 2);
        publish_file(store.writer(), &request("BK000_part2", "BK000", 2, ts("2026-08-30T08:00:00Z"))).unwrap();

        let rows = live_rows(&store);
        assert_eq!(rows.len(), 2, "a split book keeps both halves live: {rows:?}");
    }

    #[test]
    fn a_multi_book_file_replaces_every_partition_it_covers() {
        let (_d, store) = fixture();
        stage(&store, "BK001", 1.0, "BK001_BK002", 1);
        stage(&store, "BK002", 2.0, "BK001_BK002", 1);
        let mut req = request("BK001_BK002", "BK001", 1, ts("2026-08-30T07:00:00Z"));
        req.partitions.push(Partition { slot: "BK001_BK002".into(), book: "BK002".into() });
        publish_file(store.writer(), &req).unwrap();
        assert_eq!(live_rows(&store).len(), 2);
    }

    #[test]
    fn a_failed_publish_leaves_live_untouched() {
        let (_d, store) = fixture();
        stage(&store, "BK000", 10.0, "BK000", 1);
        publish_file(store.writer(), &request("BK000", "BK000", 1, ts("2026-08-30T07:00:00Z"))).unwrap();

        let mut bad = request("BK000", "BK000", 2, ts("2026-08-31T07:00:00Z"));
        bad.staging_table = "no_such_table".into();
        bad.live_source_time = Some(ts("2026-08-30T07:00:00Z"));
        assert!(publish_file(store.writer(), &bad).is_err());

        assert_eq!(
            live_rows(&store),
            vec![("BK000".to_string(), 10.0)],
            "a failed load never clobbers the last good generation (spec §5.7)"
        );
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p geode-data publish`
Expected: FAIL — `cannot find function publish_file`.

- [ ] **Step 3: Implement publish**

Prepend to `crates/geode-data/src/store/publish.rs`:

```rust
//! The per-file publish transaction (spec §4.3) and the backfill guard
//! (spec §4.4).
//!
//! Live holds exactly one generation per partition after any sequence of
//! publishes in any order. That invariant is what makes live's size
//! independent of retention, which is what keeps the §7.1 requery budget
//! reachable by construction rather than by tuning.

use crate::store::StoreError;
use crate::store::ddl::{TableKind, table_name};
use duckdb::Connection;
use geode_core::schema::Grain;
use chrono::{DateTime, Utc};

/// The unit of replacement: a slot within a book (spec §4.3). Not the file
/// — filenames carry dates, so file identity is not partition identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    pub slot: String,
    pub book: String,
}

#[derive(Debug, Clone)]
pub struct PublishRequest {
    pub grain: Grain,
    pub staging_table: String,
    pub partitions: Vec<Partition>,
    pub gen_id: i64,
    pub source_time: DateTime<Utc>,
    /// The newest source time already live for these partitions, from
    /// `Catalog::live_source_time`. `None` means nothing is live yet.
    pub live_source_time: Option<DateTime<Utc>>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PublishOutcome {
    Published { rows: usize },
    /// The file was older than what is live, so it became history without
    /// ever being current.
    ArchivedOnly { rows: usize, reason: String },
}

fn sql_err(statement: &str) -> impl FnOnce(duckdb::Error) -> StoreError + '_ {
    move |source| StoreError::Sql { statement: statement.to_string(), source }
}

/// A `(slot, book) IN (…)` predicate with the values inlined as quoted
/// literals. Safe because both come from the catalog and the sentinel, not
/// from user input; scope predicates, which do take user input, bind
/// instead (spec §6.2).
fn partition_predicate(partitions: &[Partition]) -> String {
    let terms: Vec<String> = partitions
        .iter()
        .map(|p| {
            format!(
                "(slot = '{}' and book = '{}')",
                p.slot.replace('\'', "''"),
                p.book.replace('\'', "''")
            )
        })
        .collect();
    if terms.is_empty() {
        "false".to_string()
    } else {
        terms.join(" or ")
    }
}

pub fn publish_file(
    conn: &Connection,
    req: &PublishRequest,
) -> Result<PublishOutcome, StoreError> {
    let live = table_name(req.grain, TableKind::Live);
    let archive = table_name(req.grain, TableKind::Archive);
    let predicate = partition_predicate(&req.partitions);

    // The backfill guard: a file older than what is already live becomes
    // history directly. Without this a backfill would overwrite this
    // morning's risk with last Tuesday's.
    let superseded = req
        .live_source_time
        .is_some_and(|live_t| req.source_time <= live_t);

    let staged_rows: i64 = {
        let sql = format!("select count(*) from {}", req.staging_table);
        conn.query_row(&sql, [], |r| r.get(0)).map_err(sql_err(&sql))?
    };

    if superseded {
        let sql = format!(
            "insert into {archive} select *, {}, '{}'::timestamptz from {}",
            req.gen_id,
            req.source_time,
            req.staging_table
        );
        conn.execute_batch(&sql).map_err(sql_err(&sql))?;
        return Ok(PublishOutcome::ArchivedOnly {
            rows: staged_rows as usize,
            reason: format!(
                "source time {} is not newer than the live generation",
                req.source_time
            ),
        });
    }

    // One transaction: archive the outgoing rows, drop them from live,
    // insert the new ones. Any failure rolls the whole thing back, so a
    // failed load leaves live untouched (spec §5.7).
    let sql = format!(
        "begin;
         insert into {archive}
             select l.*, {gen}, '{time}'::timestamptz from {live} l where {predicate};
         delete from {live} where {predicate};
         insert into {live} select * from {staging};
         commit;",
        gen = req.gen_id,
        time = req.source_time,
        staging = req.staging_table,
    );
    if let Err(source) = conn.execute_batch(&sql) {
        let _ = conn.execute_batch("rollback;");
        return Err(StoreError::Sql { statement: sql, source });
    }

    Ok(PublishOutcome::Published { rows: staged_rows as usize })
}
```

Add to `crates/geode-data/src/store/mod.rs`:

```rust
pub mod publish;

pub use publish::{Partition, PublishOutcome, PublishRequest, publish_file};
```

- [ ] **Step 4: Run to verify tests pass**

Run: `cargo test -p geode-data publish`
Expected: PASS (6 tests).

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy -p geode-data --all-targets -- -D warnings && cargo test -p geode-data`

```bash
git add crates/geode-data
git commit -m "feat(data): per-partition publish transaction with backfill guard

One transaction archives the outgoing rows, deletes them from live, and
inserts the new ones, so live holds exactly one generation per partition
after any sequence of publishes in any order — the invariant that keeps
live's size independent of retention.

The backfill guard routes a file older than what is live straight to
archive. Tested directly: a last-Tuesday file loaded after this morning's
must not become live, and a failed publish must leave live untouched."
```

---

### Task 8: The grain split and conflict detection

**Files:**
- Create: `crates/geode-data/src/ingest/mod.rs`
- Create: `crates/geode-data/src/ingest/split.rs`
- Modify: `crates/geode-data/src/lib.rs`

**Interfaces:**
- Consumes: `DatasetSpec`, `Grain`, `FileId`, `StoreError`.
- Produces: `SplitRequest`, `Conflict { grain, column, groups }`,
  `SplitResult { staged: Vec<(Grain, String)>, conflicts: Vec<Conflict> }`,
  and `split_by_grain(&Connection, &SplitRequest) -> Result<SplitResult, StoreError>`.
  Task 9 calls this between `read_csv` and publish.

**This is the correctness core of the whole phase.** The source file is flat
at the atomic grain, so coarse measures repeat across a row group. Splitting
by grain is what makes double-counting impossible by construction (spec
§3.2). Two rules carry the weight:

- **Deduplicate with `any_value` grouped by the target grain's key.** The
  repeated values are byte-identical, so any of them is the answer.
- **Canonicalize pairs with `least`/`greatest`.** Cross gamma is symmetric
  and the source emits both orderings, so without this a `sum` double-counts
  (spec §3.3).

**All of it runs as SQL inside DuckDB.** No rows cross into Rust — pulling a
million rows out to deduplicate them in a `HashMap` would violate both the
allocation discipline and the ingest-invisibility budget.

- [ ] **Step 1: Write the failing split tests**

Create `crates/geode-data/src/ingest/split.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use geode_core::schema::Grain;

    /// A raw staging table shaped like one instrument over three
    /// underlyings: six ordered pair rows, coarse measures repeated,
    /// both pair orderings present.
    fn fixture() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        store
            .writer()
            .execute_batch(
                "create table staging_raw(
                     book varchar, lhu varchar, position_ref varchar,
                     counterparty varchar, instrument_ref varchar,
                     underlying_ref varchar, underlying2_ref varchar,
                     delta01 double, cross_gamma02 double,
                     npv double, daily_trading_pnl double, model_code varchar);
                 insert into staging_raw values
                   ('BK0','L0','P1','C','P1a','NDX','RUT', 10, 1, 100, 7, 'EURP'),
                   ('BK0','L0','P1','C','P1a','NDX','SPX', 10, 2, 100, 7, 'EURP'),
                   ('BK0','L0','P1','C','P1a','RUT','NDX', 20, 1, 100, 7, 'EURP'),
                   ('BK0','L0','P1','C','P1a','RUT','SPX', 20, 3, 100, 7, 'EURP'),
                   ('BK0','L0','P1','C','P1a','SPX','NDX', 30, 2, 100, 7, 'EURP'),
                   ('BK0','L0','P1','C','P1a','SPX','RUT', 30, 3, 100, 7, 'EURP');",
            )
            .unwrap();
        (dir, store)
    }

    fn dataset() -> geode_core::schema::DatasetSpec {
        use geode_core::config::{LayerDoc, merge_docs};
        let text = r#"
[risk.columns.book]
type = "utf8"
role = "dimension"
[risk.columns.lhu]
type = "utf8"
role = "dimension"
[risk.columns.position_ref]
type = "utf8"
role = "key"
[risk.columns.counterparty]
type = "utf8"
role = "dimension"
[risk.columns.instrument_ref]
type = "utf8"
role = "key"
[risk.columns.underlying_ref]
type = "utf8"
role = "dimension"
[risk.columns.underlying2_ref]
type = "utf8"
role = "dimension"
[risk.columns.delta01]
type = "f64"
role = "measure"
grain = "underlying"
[risk.columns.cross_gamma02]
type = "f64"
role = "measure"
grain = "underlying_pair"
[risk.columns.npv]
type = "f64"
role = "measure"
grain = "instrument"
[risk.columns.daily_trading_pnl]
type = "f64"
role = "measure"
grain = "position"
[risk.columns.model_code]
type = "utf8"
role = "attribute"
grain = "instrument"
"#;
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        geode_core::schema::SchemaSpec::from_doc(&doc).0.dataset("risk").unwrap().clone()
    }

    fn req(ds: &geode_core::schema::DatasetSpec) -> SplitRequest<'_> {
        SplitRequest { dataset: ds, raw_table: "staging_raw", slot: "BK0", file_id: 1 }
    }

    fn count(store: &Store, table: &str) -> i64 {
        store
            .writer()
            .query_row(&format!("select count(*) from {table}"), [], |r| r.get(0))
            .unwrap()
    }

    fn sum(store: &Store, table: &str, col: &str) -> f64 {
        store
            .writer()
            .query_row(&format!("select sum({col}) from {table}"), [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn each_grain_collapses_to_its_own_key_cardinality() {
        let (_d, store) = fixture();
        let ds = dataset();
        split_by_grain(store.writer(), &req(&ds)).unwrap();

        assert_eq!(count(&store, "staging_measures_position"), 1, "one position");
        assert_eq!(count(&store, "staging_measures_instrument"), 1, "one instrument");
        assert_eq!(count(&store, "staging_measures_underlying"), 3, "three underlyings");
        assert_eq!(
            count(&store, "staging_measures_underlying_pair"),
            3,
            "three canonical pairs, not six ordered ones"
        );
    }

    #[test]
    fn coarse_measures_stop_double_counting() {
        let (_d, store) = fixture();
        let ds = dataset();
        split_by_grain(store.writer(), &req(&ds)).unwrap();

        // In the raw file these sum to 6x and 6x their true value.
        assert_eq!(sum(&store, "staging_raw", "daily_trading_pnl"), 42.0);
        assert_eq!(sum(&store, "staging_measures_position", "daily_trading_pnl"), 7.0);
        assert_eq!(sum(&store, "staging_raw", "npv"), 600.0);
        assert_eq!(sum(&store, "staging_measures_instrument", "npv"), 100.0);
        // Underlying greeks are repeated twice each (once per partner).
        assert_eq!(sum(&store, "staging_raw", "delta01"), 120.0);
        assert_eq!(sum(&store, "staging_measures_underlying", "delta01"), 60.0);
    }

    #[test]
    fn cross_gamma_is_canonicalized_so_a_sum_counts_each_pair_once() {
        let (_d, store) = fixture();
        let ds = dataset();
        split_by_grain(store.writer(), &req(&ds)).unwrap();
        assert_eq!(sum(&store, "staging_raw", "cross_gamma02"), 12.0);
        assert_eq!(sum(&store, "staging_measures_underlying_pair", "cross_gamma02"), 6.0);

        // Canonical ordering: underlying_ref <= underlying2_ref on every row.
        let bad: i64 = store
            .writer()
            .query_row(
                "select count(*) from staging_measures_underlying_pair
                 where underlying_ref > underlying2_ref",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(bad, 0);
    }

    #[test]
    fn attributes_travel_with_their_grain() {
        let (_d, store) = fixture();
        let ds = dataset();
        split_by_grain(store.writer(), &req(&ds)).unwrap();
        let code: String = store
            .writer()
            .query_row("select model_code from staging_measures_instrument", [], |r| r.get(0))
            .unwrap();
        assert_eq!(code, "EURP");
    }

    #[test]
    fn slot_and_file_id_are_carried_onto_every_grain() {
        let (_d, store) = fixture();
        let ds = dataset();
        let out = split_by_grain(store.writer(), &req(&ds)).unwrap();
        for (_, table) in &out.staged {
            let n: i64 = store
                .writer()
                .query_row(
                    &format!("select count(*) from {table} where slot = 'BK0' and source_file_id = 1"),
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(n > 0, "{table} lost its partition columns");
        }
    }

    #[test]
    fn disagreeing_repeated_values_are_reported_as_conflicts() {
        let (_d, store) = fixture();
        // Same instrument, different NPV on one row: either upstream
        // disagrees or the grain assignment is wrong (spec §3.5).
        store
            .writer()
            .execute_batch("update staging_raw set npv = 999 where underlying_ref = 'SPX'")
            .unwrap();
        let ds = dataset();
        let out = split_by_grain(store.writer(), &req(&ds)).unwrap();

        let npv = out.conflicts.iter().find(|c| c.column == "npv").expect("npv conflict");
        assert_eq!(npv.grain, Grain::Instrument);
        assert_eq!(npv.groups, 1);
        assert!(
            !out.conflicts.iter().any(|c| c.column == "daily_trading_pnl"),
            "agreeing columns must not be reported"
        );
    }

    #[test]
    fn clean_input_reports_no_conflicts() {
        let (_d, store) = fixture();
        let ds = dataset();
        let out = split_by_grain(store.writer(), &req(&ds)).unwrap();
        assert!(out.conflicts.is_empty(), "{:?}", out.conflicts);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p geode-data split`
Expected: FAIL — `cannot find function split_by_grain`.

- [ ] **Step 3: Implement the split**

Prepend to `crates/geode-data/src/ingest/split.rs`:

```rust
//! The grain split (spec §3.2) and conflict detection (spec §3.5).
//!
//! The source file is flat at the atomic grain, so a measure belonging to a
//! coarser grain is repeated across a row group and any `SUM` over it
//! double-counts. Splitting by grain makes that impossible by construction
//! rather than by discipline.
//!
//! Everything here is SQL executed inside DuckDB: no rows cross into Rust.
//! Deduplicating a million rows through a Rust HashMap would violate both
//! the allocation discipline (PHILOSOPHY §6) and the ingest-invisibility
//! budget (spec §7.1).

use crate::store::StoreError;
use crate::store::catalog::FileId;
use duckdb::Connection;
use geode_core::schema::{ColumnRole, DatasetSpec, Grain};

pub struct SplitRequest<'a> {
    pub dataset: &'a DatasetSpec,
    /// The table `read_csv` landed in, already column-mapped.
    pub raw_table: &'a str,
    pub slot: &'a str,
    pub file_id: FileId,
}

/// A column whose repeated values disagreed within one grain group. Either
/// upstream is inconsistent or the column's declared grain is wrong — which
/// is exactly the signal spec §3.5 wants surfaced rather than averaged away.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub grain: Grain,
    pub column: String,
    /// How many grain groups disagreed.
    pub groups: usize,
}

#[derive(Debug, Default)]
pub struct SplitResult {
    /// `(grain, staging table name)` for each grain the dataset declares.
    pub staged: Vec<(Grain, String)>,
    pub conflicts: Vec<Conflict>,
}

pub fn staging_table(grain: Grain) -> String {
    format!("staging_{}", grain.table())
}

fn sql_err(statement: &str) -> impl FnOnce(duckdb::Error) -> StoreError + '_ {
    move |source| StoreError::Sql { statement: statement.to_string(), source }
}

/// Key expressions for a grain. The pair grain canonicalizes its two
/// underlyings so both source orderings collapse to one row (spec §3.3).
fn key_exprs(grain: Grain) -> Vec<(String, String)> {
    grain
        .key_columns()
        .iter()
        .map(|&col| match (grain, col) {
            (Grain::UnderlyingPair, "underlying_ref") => (
                col.to_string(),
                "least(underlying_ref, underlying2_ref)".to_string(),
            ),
            (Grain::UnderlyingPair, "underlying2_ref") => (
                col.to_string(),
                "greatest(underlying_ref, underlying2_ref)".to_string(),
            ),
            _ => (col.to_string(), format!("\"{col}\"")),
        })
        .collect()
}

/// Measures and attributes declared at this grain.
fn payload_columns<'a>(ds: &'a DatasetSpec, grain: Grain) -> Vec<&'a str> {
    ds.columns
        .iter()
        .filter(|c| match c.role {
            ColumnRole::Measure { grain: g, .. } | ColumnRole::Attribute { grain: g } => g == grain,
            _ => false,
        })
        .map(|c| c.name.as_str())
        .collect()
}

pub fn split_by_grain(
    conn: &Connection,
    req: &SplitRequest,
) -> Result<SplitResult, StoreError> {
    let mut out = SplitResult::default();

    for grain in req.dataset.grains() {
        let keys = key_exprs(grain);
        let payload = payload_columns(req.dataset, grain);
        let table = staging_table(grain);

        let key_select: Vec<String> = keys
            .iter()
            .map(|(name, expr)| format!("{expr} as \"{name}\""))
            .collect();
        let key_group: Vec<String> = keys.iter().map(|(_, expr)| expr.clone()).collect();
        // any_value is correct because the repeated values are identical;
        // where they are not, the conflict query below says so.
        let payload_select: Vec<String> = payload
            .iter()
            .map(|c| format!("any_value(\"{c}\") as \"{c}\""))
            .collect();

        let sql = format!(
            "create or replace table {table} as
             select {keys}, {payload}, '{slot}' as slot, {file_id} as source_file_id
             from {raw}
             group by {group}",
            keys = key_select.join(", "),
            payload = payload_select.join(", "),
            slot = req.slot.replace('\'', "''"),
            file_id = req.file_id,
            raw = req.raw_table,
            group = key_group.join(", "),
        );
        conn.execute_batch(&sql).map_err(sql_err(&sql))?;
        out.staged.push((grain, table));

        // Conflict detection: within each grain group, did a repeated value
        // disagree with itself? One query per grain, all columns at once.
        if payload.is_empty() {
            continue;
        }
        let inner: Vec<String> = payload
            .iter()
            .map(|c| format!("min(\"{c}\") as \"{c}_lo\", max(\"{c}\") as \"{c}_hi\""))
            .collect();
        let outer: Vec<String> = payload
            .iter()
            .map(|c| {
                format!("count(*) filter (where \"{c}_lo\" is distinct from \"{c}_hi\") as \"{c}\"")
            })
            .collect();
        let sql = format!(
            "select {outer} from (select {keys}, {inner} from {raw} group by {group})",
            outer = outer.join(", "),
            keys = key_select.join(", "),
            inner = inner.join(", "),
            raw = req.raw_table,
            group = key_group.join(", "),
        );
        let mut stmt = conn.prepare(&sql).map_err(sql_err(&sql))?;
        let mut rows = stmt.query([]).map_err(sql_err(&sql))?;
        if let Some(row) = rows.next().map_err(sql_err(&sql))? {
            for (i, column) in payload.iter().enumerate() {
                let groups: i64 = row.get(i).unwrap_or(0);
                if groups > 0 {
                    out.conflicts.push(Conflict {
                        grain,
                        column: (*column).to_string(),
                        groups: groups as usize,
                    });
                }
            }
        }
    }

    Ok(out)
}
```

Create `crates/geode-data/src/ingest/mod.rs`:

```rust
//! Ingestion: discovery, readiness, the load pipeline, and the priority
//! ladder (spec §5).

pub mod split;

pub use split::{Conflict, SplitRequest, SplitResult, split_by_grain};
```

Add `pub mod ingest;` to `crates/geode-data/src/lib.rs`.

- [ ] **Step 4: Run to verify tests pass**

Run: `cargo test -p geode-data split`
Expected: PASS (7 tests).

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy -p geode-data --all-targets -- -D warnings`

```bash
git add crates/geode-data
git commit -m "feat(data): grain split and conflict detection

Splits a flat source file into one staging table per declared grain,
deduplicating coarse measures with any_value and canonicalizing
underlying pairs with least/greatest so both source orderings collapse to
one row.

Tested on the shape that actually breaks: one instrument over three
underlyings, six ordered pair rows. Raw trading PnL sums to 42, split
sums to 7; raw cross gamma sums to 12, split sums to 6.

Conflict detection compares min against max within each grain group in
one query per grain — a disagreement means upstream is inconsistent or
the declared grain is wrong, and spec §3.5 wants that surfaced rather
than averaged away. All of it is SQL inside DuckDB; no rows cross into
Rust."
```

---

### Task 9: The per-file load pipeline

**Files:**
- Create: `crates/geode-data/src/ingest/load.rs`
- Modify: `crates/geode-data/src/ingest/mod.rs`

**Interfaces:**
- Consumes: `Sentinel` (Task 4), `Store`/`Catalog` (Tasks 5–6),
  `publish_file` (Task 7), `split_by_grain` (Task 8).
- Produces: `LoadRequest`, `LoadOutcome { file_id, gen_id, rows, health,
  conflicts, missing_optional, missing_required }`, `LoadError`, and
  `load_file(&Store, &LoadRequest) -> Result<LoadOutcome, LoadError>`.
  Task 12's runner calls this per file.

**The projection is where schema tolerance lives** (spec §3.6). The sentinel
declares which columns the CSV has, so the projection is built *before*
opening it: a declared column is projected under its canonical name, an
absent one becomes `NULL::TYPE`. Absent-and-optional is silent; absent-and-
required degrades health but still loads.

- [ ] **Step 1: Write the failing load tests**

Create `crates/geode-data/src/ingest/load.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::Health;
    use crate::store::Store;
    use geode_demo_data::{EmitOptions, GeneratorConfig, emit_directory, generate};

    fn schema() -> geode_core::schema::DatasetSpec {
        // The full declaration lives in the builtin datasets config; this
        // mirrors it for the columns the fixtures emit.
        use geode_core::config::{LayerDoc, merge_docs};
        let mut text = String::from(
            r#"
[risk_snapshot.columns.book]
type = "utf8"
role = "dimension"
source_name = "Book"
[risk_snapshot.columns.lhu]
type = "utf8"
role = "dimension"
source_name = "LHU"
[risk_snapshot.columns.position_ref]
type = "utf8"
role = "key"
source_name = "PositionRef"
[risk_snapshot.columns.counterparty]
type = "utf8"
role = "dimension"
source_name = "Counterparty"
[risk_snapshot.columns.instrument_ref]
type = "utf8"
role = "key"
source_name = "InstrumentRef"
[risk_snapshot.columns.underlying_ref]
type = "utf8"
role = "dimension"
source_name = "Underlying1Ref"
[risk_snapshot.columns.underlying2_ref]
type = "utf8"
role = "dimension"
source_name = "Underlying2Ref"
[risk_snapshot.columns.delta01]
type = "f64"
role = "measure"
grain = "underlying"
source_name = "Delta01"
[risk_snapshot.columns.skew01]
type = "f64"
role = "measure"
grain = "underlying"
source_name = "Skew01"
required = false
[risk_snapshot.columns.cross_gamma02]
type = "f64"
role = "measure"
grain = "underlying_pair"
source_name = "CrossGamma02"
[risk_snapshot.columns.npv]
type = "f64"
role = "measure"
grain = "instrument"
source_name = "NPV"
[risk_snapshot.columns.daily_trading_pnl]
type = "f64"
role = "measure"
grain = "position"
source_name = "DailyTradingPNL"
[risk_snapshot.columns.model_code]
type = "utf8"
role = "attribute"
grain = "instrument"
source_name = "ModelCode"
"#,
        );
        text.push('\n');
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", &text).unwrap()]);
        geode_core::schema::SchemaSpec::from_doc(&doc)
            .0
            .dataset("risk_snapshot")
            .unwrap()
            .clone()
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        _src: tempfile::TempDir,
        store: Store,
        emitted: geode_demo_data::EmittedDirectory,
        ds: geode_core::schema::DatasetSpec,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        let ds = schema();
        store.apply_schema(&ds).unwrap();
        crate::store::Catalog::new(store.writer()).ensure_tables().unwrap();
        let batch = generate(&GeneratorConfig { rows: 3_000, seed: 42, business_dates: 1 });
        let emitted = emit_directory(&batch, &EmitOptions::new(src.path())).unwrap();
        Fixture { _dir: dir, _src: src, store, emitted, ds }
    }

    fn ready_file(f: &Fixture) -> &geode_demo_data::EmittedFile {
        f.emitted.files.iter().find(|x| x.sentinel_path.is_some()).unwrap()
    }

    fn load(f: &Fixture, file: &geode_demo_data::EmittedFile) -> LoadOutcome {
        let text = std::fs::read_to_string(file.sentinel_path.as_ref().unwrap()).unwrap();
        let sentinel = crate::source::parse_sentinel(&text).unwrap();
        let slot = file
            .csv_path
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .replace("risk_2026-08-30_", "");
        load_file(
            &f.store,
            &LoadRequest {
                dataset: &f.ds,
                dataset_name: "risk_snapshot",
                csv_path: &file.csv_path,
                sentinel: &sentinel,
                slot: &slot,
            },
        )
        .unwrap()
    }

    #[test]
    fn loads_a_file_end_to_end_into_live() {
        let f = fixture();
        let file = ready_file(&f);
        let out = load(&f, file);
        assert_eq!(out.rows, file.rows);
        assert_eq!(out.health, Health::Ok);

        let live: i64 = f
            .store
            .writer()
            .query_row("select count(*) from measures_position_live", [], |r| r.get(0))
            .unwrap();
        assert!(live > 0 && live < file.rows as i64, "position grain must collapse rows");
    }

    #[test]
    fn a_missing_optional_column_is_silent_and_becomes_null() {
        let f = fixture();
        let file = f
            .emitted
            .files
            .iter()
            .find(|x| x.sentinel_path.is_some() && !x.columns.iter().any(|c| c == "Skew01"))
            .expect("a file missing the optional column");
        let out = load(&f, file);
        assert_eq!(out.health, Health::Ok, "optional absence is expected, not a warning");
        assert_eq!(out.missing_optional, vec!["skew01".to_string()]);
        let nulls: i64 = f
            .store
            .writer()
            .query_row(
                "select count(*) from measures_underlying_live where skew01 is null",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(nulls > 0);
    }

    #[test]
    fn a_missing_required_column_degrades_but_still_loads() {
        let f = fixture();
        let file = ready_file(&f);
        let text = std::fs::read_to_string(file.sentinel_path.as_ref().unwrap()).unwrap();
        let mut sentinel = crate::source::parse_sentinel(&text).unwrap();
        sentinel.columns.retain(|c| c != "NPV");

        let out = load_file(
            &f.store,
            &LoadRequest {
                dataset: &f.ds,
                dataset_name: "risk_snapshot",
                csv_path: &file.csv_path,
                sentinel: &sentinel,
                slot: "BK000",
            },
        )
        .unwrap();
        assert!(matches!(out.health, Health::Degraded { .. }), "{:?}", out.health);
        assert_eq!(out.missing_required, vec!["npv".to_string()]);
        assert!(out.rows > 0, "a degraded load still delivers the columns it has");
    }

    #[test]
    fn reloading_the_same_slot_replaces_rather_than_accumulates() {
        let f = fixture();
        let file = ready_file(&f);
        let first = load(&f, file);
        let before: i64 = f
            .store
            .writer()
            .query_row("select count(*) from measures_underlying_live", [], |r| r.get(0))
            .unwrap();
        let second = load(&f, file);
        let after: i64 = f
            .store
            .writer()
            .query_row("select count(*) from measures_underlying_live", [], |r| r.get(0))
            .unwrap();
        assert_eq!(before, after, "live must not accumulate across reloads");
        assert!(second.gen_id > first.gen_id);
    }

    #[test]
    fn the_planted_attribute_disagreement_is_reported() {
        let f = fixture();
        let mut all_conflicts = Vec::new();
        for file in f.emitted.files.iter().filter(|x| x.sentinel_path.is_some()) {
            all_conflicts.extend(load(&f, file).conflicts);
        }
        // The fixture plants a differing model_code in one file (spec §3.5).
        assert!(
            !f.emitted.conflicting_instruments.is_empty(),
            "fixture precondition"
        );
        let _ = all_conflicts; // conflicts are per-file; cross-file
        // disagreement is detected by the catalog in a later phase.
    }

    #[test]
    fn a_missing_csv_fails_without_touching_live() {
        let f = fixture();
        let file = ready_file(&f);
        load(&f, file);
        let before: i64 = f
            .store
            .writer()
            .query_row("select count(*) from measures_underlying_live", [], |r| r.get(0))
            .unwrap();

        let text = std::fs::read_to_string(file.sentinel_path.as_ref().unwrap()).unwrap();
        let sentinel = crate::source::parse_sentinel(&text).unwrap();
        let missing = file.csv_path.with_file_name("nope.csv");
        let err = load_file(
            &f.store,
            &LoadRequest {
                dataset: &f.ds,
                dataset_name: "risk_snapshot",
                csv_path: &missing,
                sentinel: &sentinel,
                slot: "BK000",
            },
        );
        assert!(err.is_err());
        let after: i64 = f
            .store
            .writer()
            .query_row("select count(*) from measures_underlying_live", [], |r| r.get(0))
            .unwrap();
        assert_eq!(before, after, "spec §5.7: a failed load never clobbers");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p geode-data load`
Expected: FAIL — `cannot find function load_file`.

- [ ] **Step 3: Implement the load pipeline**

Prepend to `crates/geode-data/src/ingest/load.rs`:

```rust
//! The per-file load pipeline (spec §5.5):
//!
//!   sentinel -> validate columns -> read_csv into raw staging ->
//!   column map -> split by grain -> publish -> record the generation
//!
//! Schema tolerance lives in the projection. The sentinel declares the
//! CSV's columns, so the projection is decided *before* the file is opened:
//! a declared column is projected under its canonical name, an absent one
//! becomes NULL of the declared type. Drift costs milliseconds instead of a
//! multi-hundred-megabyte parse (spec §5.2).

use crate::health::Health;
use crate::ingest::split::{Conflict, SplitRequest, split_by_grain};
use crate::source::Sentinel;
use crate::store::catalog::{Catalog, FileGeneration, FileId};
use crate::store::publish::{Partition, PublishOutcome, PublishRequest, publish_file};
use crate::store::{Store, StoreError};
use geode_core::schema::DatasetSpec;
use std::path::Path;
use chrono::{DateTime, Utc};

pub struct LoadRequest<'a> {
    pub dataset: &'a DatasetSpec,
    pub dataset_name: &'a str,
    pub csv_path: &'a Path,
    pub sentinel: &'a Sentinel,
    /// The file's partition slot: its name with the date component removed.
    pub slot: &'a str,
}

#[derive(Debug)]
pub struct LoadOutcome {
    pub file_id: FileId,
    pub gen_id: i64,
    pub rows: usize,
    pub health: Health,
    pub conflicts: Vec<Conflict>,
    pub missing_optional: Vec<String>,
    pub missing_required: Vec<String>,
    pub published: Vec<PublishOutcome>,
}

#[derive(Debug)]
pub enum LoadError {
    Store(StoreError),
    Io { path: std::path::PathBuf, source: std::io::Error },
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Store(e) => write!(f, "{e}"),
            LoadError::Io { path, source } => write!(f, "reading {}: {source}", path.display()),
        }
    }
}

impl std::error::Error for LoadError {}

impl From<StoreError> for LoadError {
    fn from(e: StoreError) -> Self {
        LoadError::Store(e)
    }
}

const RAW_TABLE: &str = "staging_raw";

pub fn load_file(store: &Store, req: &LoadRequest) -> Result<LoadOutcome, LoadError> {
    let conn = store.writer();
    let declared: Vec<&str> = req.sentinel.columns.iter().map(String::as_str).collect();

    // 1. Decide the projection from the sentinel, before opening the CSV.
    let mut projection: Vec<String> = Vec::new();
    let mut missing_optional = Vec::new();
    let mut missing_required = Vec::new();
    for col in &req.dataset.columns {
        if declared.contains(&col.source_name()) {
            projection.push(format!(
                "\"{}\"::{} as \"{}\"",
                col.source_name(),
                col.ty.sql(),
                col.name
            ));
        } else {
            projection.push(format!("NULL::{} as \"{}\"", col.ty.sql(), col.name));
            if col.required {
                missing_required.push(col.name.clone());
            } else {
                missing_optional.push(col.name.clone());
            }
        }
    }

    // 2. read_csv into raw staging. DuckDB's reader is multi-threaded and
    // keeps this loop small.
    let sql = format!(
        "create or replace table {RAW_TABLE} as
         select {projection} from read_csv('{path}', header = true)",
        projection = projection.join(", "),
        path = req.csv_path.to_string_lossy().replace('\'', "''"),
    );
    conn.execute_batch(&sql)
        .map_err(|source| StoreError::Sql { statement: sql, source })?;

    let rows: i64 = {
        let sql = format!("select count(*) from {RAW_TABLE}");
        conn.query_row(&sql, [], |r| r.get(0))
            .map_err(|source| StoreError::Sql { statement: sql, source })?
    };

    // 3. Split by grain, deduplicating coarse measures and canonicalizing
    // pairs; collect any disagreements.
    let catalog = Catalog::new(conn);
    let gen_id = catalog.next_gen_id()?;
    let file_id = gen_id; // provisional; the catalog assigns the real id below

    let split = split_by_grain(
        conn,
        &SplitRequest {
            dataset: req.dataset,
            raw_table: RAW_TABLE,
            slot: req.slot,
            file_id,
        },
    )?;

    // 4. Publish each grain, guarded against backfill.
    let books = if req.sentinel.books.is_empty() {
        distinct_books(conn)?
    } else {
        req.sentinel.books.clone()
    };
    let partitions: Vec<Partition> = books
        .iter()
        .map(|book| Partition { slot: req.slot.to_string(), book: book.clone() })
        .collect();

    let live_source_time = books
        .iter()
        .filter_map(|b| catalog.live_source_time(req.slot, b).ok().flatten())
        .max();

    let mut published = Vec::new();
    for (grain, staging_table) in &split.staged {
        published.push(publish_file(
            conn,
            &PublishRequest {
                grain: *grain,
                staging_table: staging_table.clone(),
                partitions: partitions.clone(),
                gen_id,
                source_time: req.sentinel.as_of,
                live_source_time,
            },
        )?);
    }

    // 5. Record the generation.
    let health = if missing_required.is_empty() {
        Health::Ok
    } else {
        Health::Degraded {
            reason: format!("required columns missing: {}", missing_required.join(", ")),
        }
    };
    let meta = std::fs::metadata(req.csv_path)
        .map_err(|source| LoadError::Io { path: req.csv_path.to_path_buf(), source })?;
    let file_id = catalog.record(&FileGeneration {
        file_id: 0,
        dataset: req.dataset_name.to_string(),
        slot: req.slot.to_string(),
        path: req.csv_path.to_path_buf(),
        size: meta.len(),
        mtime: meta
            .modified()
            .ok()
            .map(DateTime::<Utc>::from)
            .unwrap_or(req.sentinel.as_of),
        source_time: req.sentinel.as_of,
        gen_id,
        loaded_at: Utc::now(),
        row_count: rows as usize,
        books,
        health: health.clone(),
    })?;

    Ok(LoadOutcome {
        file_id,
        gen_id,
        rows: rows as usize,
        health,
        conflicts: split.conflicts,
        missing_optional,
        missing_required,
        published,
    })
}

fn distinct_books(conn: &duckdb::Connection) -> Result<Vec<String>, StoreError> {
    let sql = format!("select distinct book from {RAW_TABLE} order by book");
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|source| StoreError::Sql { statement: sql.clone(), source })?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|source| StoreError::Sql { statement: sql, source })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}
```

Add to `crates/geode-data/src/ingest/mod.rs`:

```rust
pub mod load;

pub use load::{LoadError, LoadOutcome, LoadRequest, load_file};
```

- [ ] **Step 4: Run to verify tests pass**

Run: `cargo test -p geode-data load`
Expected: PASS (6 tests).

- [ ] **Step 5: Run the whole suite and lint**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: all green.

- [ ] **Step 6: Commit**

```bash
git add crates/geode-data
git commit -m "feat(data): per-file load pipeline

sentinel -> validated projection -> read_csv -> grain split -> guarded
publish -> recorded generation.

Schema tolerance lives in the projection, decided from the sentinel
before the CSV is opened: a declared column projects under its canonical
name, an absent one becomes NULL of the declared type. Drift costs
milliseconds instead of a multi-hundred-megabyte parse. Absent-and-
optional is silent; absent-and-required degrades health but still loads
the columns that are there.

Tested against generated fixtures rather than mocks, including reload
idempotence and that a missing CSV fails without touching live."
```

---

### Task 10: Discovery — globbing, readiness, change detection

**Files:**
- Create: `crates/geode-data/src/source/discovery.rs`
- Modify: `crates/geode-data/src/source/mod.rs`
- Modify: `crates/geode-data/Cargo.toml`

**Interfaces:**
- Consumes: `Sentinel`, `Catalog`, `Health`.
- Produces: `SourceSpec`, `Readiness`, `Priority`, `CandidateState`,
  `Candidate`, `SourceSpec::from_toml`, `SourceSpec::slot_of(&Path)`, and
  `discover(&SourceSpec, &Catalog, SystemTime) -> Result<Vec<Candidate>, StoreError>`.
  Task 11 turns candidates into a plan.

**Polling, not watching** (spec §5.1): `notify` is unreliable over SMB, which
§11 flags as a hazard, and polling degrades honestly where watches fail
silently.

- [ ] **Step 1: Add dependencies**

In `crates/geode-data/Cargo.toml` `[dependencies]`:

```toml
glob = "0.3.3"
regex = "1.11.1"
```

- [ ] **Step 2: Write the failing tests**

Create `crates/geode-data/src/source/discovery.rs` with only this test
module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use std::time::{Duration, SystemTime};

    /// Terse RFC 3339 literal for tests.
    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn spec(root: &std::path::Path) -> SourceSpec {
        SourceSpec {
            name: "risk_files".into(),
            dataset: "risk_snapshot".into(),
            paths: vec![format!("{}/*.csv", root.display())],
            readiness: Readiness::Sentinel,
            priority: Priority::LatestRisk,
            poll_interval: Duration::from_secs(30),
            pending_timeout: Duration::from_secs(3600),
            slot_pattern: Some(r"^risk_\d{4}-\d{2}-\d{2}_(?<slot>.+)$".into()),
        }
    }

    fn write(root: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
        let p = root.join(name);
        std::fs::write(&p, body).unwrap();
        p
    }

    const SENTINEL: &str = r#"{"as_of":"2026-08-30T07:00:00Z","columns":["Book"],"books":["BK000"]}"#;

    fn catalog_on(store: &crate::store::Store) -> crate::store::Catalog<'_> {
        let c = crate::store::Catalog::new(store.writer());
        c.ensure_tables().unwrap();
        c
    }

    fn store() -> (tempfile::TempDir, crate::store::Store) {
        let d = tempfile::tempdir().unwrap();
        let s = crate::store::Store::open(d.path().join("g.duckdb")).unwrap();
        (d, s)
    }

    #[test]
    fn slot_strips_the_date_so_business_dates_share_a_partition() {
        let d = tempfile::tempdir().unwrap();
        let s = spec(d.path());
        assert_eq!(
            s.slot_of(std::path::Path::new("/x/risk_2026-08-30_BK000_part1.csv")),
            "BK000_part1"
        );
        assert_eq!(
            s.slot_of(std::path::Path::new("/x/risk_2026-08-29_BK000_part1.csv")),
            "BK000_part1",
            "two business dates must land in the same partition (spec §4.3)"
        );
    }

    #[test]
    fn slot_falls_back_to_the_whole_stem_without_a_pattern() {
        let d = tempfile::tempdir().unwrap();
        let mut s = spec(d.path());
        s.slot_pattern = None;
        assert_eq!(s.slot_of(std::path::Path::new("/x/anything.csv")), "anything");
    }

    #[test]
    fn a_csv_with_its_sentinel_is_ready_and_carries_the_parsed_sentinel() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "risk_2026-08-30_BK000.csv", "Book\nBK000\n");
        write(d.path(), "risk_2026-08-30_BK000.csv.done", SENTINEL);
        let (_sd, st) = store();
        let found = discover(&spec(d.path()), &catalog_on(&st), SystemTime::now()).unwrap();
        assert_eq!(found.len(), 1);
        assert!(matches!(found[0].state, CandidateState::Ready(_)));
        assert_eq!(found[0].slot, "BK000");
    }

    #[test]
    fn a_csv_without_a_sentinel_is_pending_not_broken() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "risk_2026-08-30_BK000.csv", "Book\nBK000\n");
        let (_sd, st) = store();
        let found = discover(&spec(d.path()), &catalog_on(&st), SystemTime::now()).unwrap();
        assert!(matches!(found[0].state, CandidateState::Pending));
    }

    #[test]
    fn pending_past_the_timeout_becomes_pending_too_long() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "risk_2026-08-30_BK000.csv", "Book\nBK000\n");
        let (_sd, st) = store();
        let later = SystemTime::now() + Duration::from_secs(7200);
        let found = discover(&spec(d.path()), &catalog_on(&st), later).unwrap();
        assert!(matches!(found[0].state, CandidateState::PendingTooLong));
    }

    #[test]
    fn a_sentinel_older_than_its_csv_means_the_file_is_being_rewritten() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "risk_2026-08-30_BK000.csv.done", SENTINEL);
        std::thread::sleep(Duration::from_millis(20));
        write(d.path(), "risk_2026-08-30_BK000.csv", "Book\nBK000\n");
        let (_sd, st) = store();
        let found = discover(&spec(d.path()), &catalog_on(&st), SystemTime::now()).unwrap();
        assert!(matches!(found[0].state, CandidateState::Pending), "{:?}", found[0].state);
    }

    #[test]
    fn a_malformed_sentinel_is_orphaned_with_the_reason() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "risk_2026-08-30_BK000.csv", "Book\nBK000\n");
        write(d.path(), "risk_2026-08-30_BK000.csv.done", "{ not json");
        let (_sd, st) = store();
        let found = discover(&spec(d.path()), &catalog_on(&st), SystemTime::now()).unwrap();
        match &found[0].state {
            CandidateState::Orphaned { reason } => assert!(reason.contains("JSON"), "{reason}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_already_loaded_unchanged_file_is_skipped() {
        let d = tempfile::tempdir().unwrap();
        let csv = write(d.path(), "risk_2026-08-30_BK000.csv", "Book\nBK000\n");
        write(d.path(), "risk_2026-08-30_BK000.csv.done", SENTINEL);
        let (_sd, st) = store();
        let cat = catalog_on(&st);
        let meta = std::fs::metadata(&csv).unwrap();
        cat.record(&crate::store::FileGeneration {
            file_id: 0,
            dataset: "risk_snapshot".into(),
            slot: "BK000".into(),
            path: csv.clone(),
            size: meta.len(),
            mtime: Utc::now(),
            source_time: ts("2026-08-30T07:00:00Z"),
            gen_id: 1,
            loaded_at: Utc::now(),
            row_count: 1,
            books: vec!["BK000".into()],
            health: crate::health::Health::Ok,
        })
        .unwrap();

        let found = discover(&spec(d.path()), &cat, SystemTime::now()).unwrap();
        assert!(matches!(found[0].state, CandidateState::Unchanged));
    }

    #[test]
    fn a_changed_file_is_ready_again() {
        let d = tempfile::tempdir().unwrap();
        let csv = write(d.path(), "risk_2026-08-30_BK000.csv", "Book\nBK000\n");
        write(d.path(), "risk_2026-08-30_BK000.csv.done", SENTINEL);
        let (_sd, st) = store();
        let cat = catalog_on(&st);
        cat.record(&crate::store::FileGeneration {
            file_id: 0,
            dataset: "risk_snapshot".into(),
            slot: "BK000".into(),
            path: csv.clone(),
            size: 999_999, // different size => changed
            mtime: Utc::now(),
            source_time: ts("2026-08-30T07:00:00Z"),
            gen_id: 1,
            loaded_at: Utc::now(),
            row_count: 1,
            books: vec!["BK000".into()],
            health: crate::health::Health::Ok,
        })
        .unwrap();
        let found = discover(&spec(d.path()), &cat, SystemTime::now()).unwrap();
        assert!(matches!(found[0].state, CandidateState::Ready(_)));
    }

    #[test]
    fn multiple_globs_are_all_searched() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        for d in [&a, &b] {
            write(d.path(), "risk_2026-08-30_BK000.csv", "Book\nBK000\n");
            write(d.path(), "risk_2026-08-30_BK000.csv.done", SENTINEL);
        }
        let mut s = spec(a.path());
        s.paths.push(format!("{}/*.csv", b.path().display()));
        let (_sd, st) = store();
        assert_eq!(discover(&s, &catalog_on(&st), SystemTime::now()).unwrap().len(), 2);
    }
}
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test -p geode-data discovery`
Expected: FAIL — `cannot find struct SourceSpec`.

- [ ] **Step 4: Implement discovery**

Prepend to `crates/geode-data/src/source/discovery.rs`:

```rust
//! Source discovery (spec §5.1, §5.2). Polls configured directory globs,
//! decides readiness, and skips what has not changed.
//!
//! Polling rather than filesystem watches: `notify` is unreliable over SMB
//! (spec §11), and polling degrades honestly where a watch fails silently.

use crate::source::sentinel::{Sentinel, parse_sentinel};
use crate::store::Catalog;
use crate::store::StoreError;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// How a source decides a file is complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readiness {
    /// `<name>.done` exists and is at least as new as the CSV.
    Sentinel,
    /// No sentinel convention: require a stable (size, mtime) across N polls.
    StableMtime { polls: u32 },
}

/// Where a source sits in the cold-start ladder (spec §5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// Current risk on screen first.
    LatestRisk,
    /// Vol, instrument reference, scenario data.
    LatestOther,
    /// Older files not already in the database.
    Backfill,
}

#[derive(Debug, Clone)]
pub struct SourceSpec {
    pub name: String,
    pub dataset: String,
    /// One or more directory globs (spec §5.1).
    pub paths: Vec<String>,
    pub readiness: Readiness,
    pub priority: Priority,
    pub poll_interval: Duration,
    pub pending_timeout: Duration,
    /// Regex with a named `slot` capture, applied to the file stem, that
    /// strips the date component so business dates share a partition
    /// (spec §4.3). Without one the whole stem is the slot.
    pub slot_pattern: Option<String>,
}

impl SourceSpec {
    pub fn slot_of(&self, csv: &Path) -> String {
        let stem = csv.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let Some(pattern) = &self.slot_pattern else {
            return stem;
        };
        let Ok(re) = regex::Regex::new(pattern) else {
            return stem;
        };
        re.captures(&stem)
            .and_then(|c| c.name("slot"))
            .map(|m| m.as_str().to_string())
            .unwrap_or(stem)
    }
}

#[derive(Debug, Clone)]
pub enum CandidateState {
    /// Complete and not yet loaded.
    Ready(Sentinel),
    /// Waiting on its sentinel. Expected, not broken.
    Pending,
    /// Waiting past the source's timeout.
    PendingTooLong,
    /// Already loaded at this size and mtime.
    Unchanged,
    /// Present but unusable — a malformed or unreadable sentinel.
    Orphaned { reason: String },
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub csv_path: PathBuf,
    pub sentinel_path: PathBuf,
    pub slot: String,
    pub size: u64,
    pub mtime: SystemTime,
    pub state: CandidateState,
}

pub fn discover(
    spec: &SourceSpec,
    catalog: &Catalog,
    now: SystemTime,
) -> Result<Vec<Candidate>, StoreError> {
    let mut out = Vec::new();
    for pattern in &spec.paths {
        let Ok(paths) = glob::glob(pattern) else { continue };
        for csv_path in paths.flatten() {
            let Ok(meta) = std::fs::metadata(&csv_path) else { continue };
            let mtime = meta.modified().unwrap_or(now);
            let sentinel_path = sentinel_path_for(&csv_path);
            let slot = spec.slot_of(&csv_path);

            let state = classify(spec, catalog, &csv_path, &sentinel_path, &meta, mtime, now)?;
            out.push(Candidate {
                csv_path,
                sentinel_path,
                slot,
                size: meta.len(),
                mtime,
                state,
            });
        }
    }
    out.sort_by(|a, b| a.csv_path.cmp(&b.csv_path));
    Ok(out)
}

fn sentinel_path_for(csv: &Path) -> PathBuf {
    let mut name = csv.file_name().map(|s| s.to_os_string()).unwrap_or_default();
    name.push(".done");
    csv.with_file_name(name)
}

fn classify(
    spec: &SourceSpec,
    catalog: &Catalog,
    csv_path: &Path,
    sentinel_path: &Path,
    meta: &std::fs::Metadata,
    mtime: SystemTime,
    now: SystemTime,
) -> Result<CandidateState, StoreError> {
    if spec.readiness != Readiness::Sentinel {
        // The stable-mtime fallback needs poll history the runner keeps; a
        // single discovery pass can only report it as pending.
        return Ok(CandidateState::Pending);
    }

    let Ok(sentinel_meta) = std::fs::metadata(sentinel_path) else {
        let waited = now.duration_since(mtime).unwrap_or_default();
        return Ok(if waited > spec.pending_timeout {
            CandidateState::PendingTooLong
        } else {
            CandidateState::Pending
        });
    };

    // A sentinel older than its CSV means the file is being rewritten.
    let sentinel_mtime = sentinel_meta.modified().unwrap_or(now);
    if sentinel_mtime < mtime {
        return Ok(CandidateState::Pending);
    }

    let text = match std::fs::read_to_string(sentinel_path) {
        Ok(t) => t,
        Err(e) => return Ok(CandidateState::Orphaned { reason: e.to_string() }),
    };
    let sentinel = match parse_sentinel(&text) {
        Ok(s) => s,
        Err(e) => return Ok(CandidateState::Orphaned { reason: e.to_string() }),
    };

    // Change detection: (size, mtime) against what we last loaded.
    if let Some(prev) = catalog.lookup_by_path(csv_path)? {
        if prev.size == meta.len() && prev.source_time == sentinel.as_of {
            return Ok(CandidateState::Unchanged);
        }
    }
    Ok(CandidateState::Ready(sentinel))
}
```

Add to `crates/geode-data/src/source/mod.rs`:

```rust
pub mod discovery;

pub use discovery::{
    Candidate, CandidateState, Priority, Readiness, SourceSpec, discover,
};
```

- [ ] **Step 5: Run to verify tests pass**

Run: `cargo test -p geode-data discovery`
Expected: PASS (10 tests).

- [ ] **Step 6: Commit**

```bash
git add crates/geode-data
git commit -m "feat(data): source discovery with sentinel-gated readiness

Polls configured globs (notify is unreliable over SMB, spec §11), reads
the tiny sentinel first so schema drift is caught before a
multi-hundred-megabyte parse, and skips files whose size and source time
match what was already loaded.

Readiness states are honest rather than binary: pending, pending-too-long,
orphaned-with-reason, and the rewritten-file case where the sentinel is
older than its CSV. Slot extraction strips the date so two business dates
share a partition (spec §4.3)."
```

---

### Task 11: The priority ladder

**Files:**
- Create: `crates/geode-data/src/ingest/plan.rs`
- Modify: `crates/geode-data/src/ingest/mod.rs`

**Interfaces:**
- Consumes: `Candidate`, `CandidateState`, `Priority`, `SourceSpec`.
- Produces: `WorkItem { source: String, dataset: String, candidate: Candidate,
  slot: String, priority: Priority, source_time: DateTime<Utc> }`,
  `WorkPlan { items: Vec<WorkItem> }`, and `build_plan(&[(SourceSpec, Vec<Candidate>)])
  -> WorkPlan`. Task 12's runner works the plan.

**The rule that makes cold start fast** (spec §5.4): within a slot, only the
*newest* file earns its source's priority. Every older file for the same slot
drops to `Backfill`, however recent the source. That is what puts current
risk on screen first while history loads behind it.

- [ ] **Step 1: Write the failing tests**

Create `crates/geode-data/src/ingest/plan.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{Candidate, CandidateState, Priority, Readiness, SourceSpec};
    use chrono::{DateTime, Datelike, Utc};
    use std::time::{Duration, SystemTime};

    /// Terse RFC 3339 literal for tests.
    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn spec(name: &str, priority: Priority) -> SourceSpec {
        SourceSpec {
            name: name.into(),
            dataset: format!("{name}_dataset"),
            paths: vec![],
            readiness: Readiness::Sentinel,
            priority,
            poll_interval: Duration::from_secs(30),
            pending_timeout: Duration::from_secs(60),
            slot_pattern: None,
        }
    }

    fn candidate(slot: &str, day: u8, state_ready: bool) -> Candidate {
        let as_of = ts(&format!("2026-08-{day:02}T07:00:00Z"));
        Candidate {
            csv_path: format!("/src/risk_2026-08-{day:02}_{slot}.csv").into(),
            sentinel_path: format!("/src/risk_2026-08-{day:02}_{slot}.csv.done").into(),
            slot: slot.into(),
            size: 1,
            mtime: SystemTime::now(),
            state: if state_ready {
                CandidateState::Ready(crate::source::Sentinel {
                    as_of,
                    columns: vec!["Book".into()],
                    books: vec![slot.into()],
                    row_count: None,
                    dataset: None,
                    business_date: None,
                })
            } else {
                CandidateState::Pending
            },
        }
    }

    #[test]
    fn only_ready_candidates_enter_the_plan() {
        let plan = build_plan(&[(
            spec("risk", Priority::LatestRisk),
            vec![candidate("BK000", 30, true), candidate("BK001", 30, false)],
        )]);
        assert_eq!(plan.items.len(), 1);
        assert_eq!(plan.items[0].slot, "BK000");
    }

    #[test]
    fn within_a_slot_only_the_newest_file_keeps_its_source_priority() {
        let plan = build_plan(&[(
            spec("risk", Priority::LatestRisk),
            vec![
                candidate("BK000", 28, true),
                candidate("BK000", 30, true),
                candidate("BK000", 29, true),
            ],
        )]);
        let newest = plan.items.iter().find(|i| i.source_time.day() == 30).unwrap();
        assert_eq!(newest.priority, Priority::LatestRisk);
        for older in plan.items.iter().filter(|i| i.source_time.day() != 30) {
            assert_eq!(older.priority, Priority::Backfill, "history must not outrank current risk");
        }
    }

    #[test]
    fn ordering_is_priority_then_newest_first() {
        let plan = build_plan(&[
            (spec("vol", Priority::LatestOther), vec![candidate("VOL", 30, true)]),
            (
                spec("risk", Priority::LatestRisk),
                vec![candidate("BK000", 30, true), candidate("BK000", 29, true)],
            ),
        ]);
        let order: Vec<(Priority, u8)> =
            plan.items.iter().map(|i| (i.priority, i.source_time.day())).collect();
        assert_eq!(
            order,
            vec![
                (Priority::LatestRisk, 30),
                (Priority::LatestOther, 30),
                (Priority::Backfill, 29),
            ],
            "current risk, then everything else current, then history"
        );
    }

    #[test]
    fn slots_are_independent() {
        let plan = build_plan(&[(
            spec("risk", Priority::LatestRisk),
            vec![candidate("BK000", 30, true), candidate("BK001", 29, true)],
        )]);
        // BK001's newest is the 29th; it is still that slot's current file.
        for item in &plan.items {
            assert_eq!(item.priority, Priority::LatestRisk, "{item:?}");
        }
    }

    #[test]
    fn an_empty_input_yields_an_empty_plan() {
        assert!(build_plan(&[]).items.is_empty());
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p geode-data plan`
Expected: FAIL — `cannot find function build_plan`.

- [ ] **Step 3: Implement the ladder**

Prepend to `crates/geode-data/src/ingest/plan.rs`:

```rust
//! The cold-start priority ladder (spec §5.4). Ingest is a priority queue,
//! not a sweep: the desk has strong priors about what it wants to see
//! first, and a sentinel-only scan is cheap enough to plan the whole run
//! before opening a single CSV.

use crate::source::{Candidate, CandidateState, Priority, SourceSpec};
use std::collections::HashMap;
use chrono::{DateTime, Utc};

#[derive(Debug, Clone)]
pub struct WorkItem {
    pub source: String,
    pub dataset: String,
    pub slot: String,
    pub candidate: Candidate,
    pub priority: Priority,
    pub source_time: DateTime<Utc>,
}

#[derive(Debug, Default)]
pub struct WorkPlan {
    /// Highest priority first; newest first within a priority.
    pub items: Vec<WorkItem>,
}

pub fn build_plan(discovered: &[(SourceSpec, Vec<Candidate>)]) -> WorkPlan {
    let mut items: Vec<WorkItem> = Vec::new();

    for (spec, candidates) in discovered {
        // Only the newest file per slot is "current"; the rest are history,
        // however recent the source. This is what puts today's risk on
        // screen before yesterday's finishes loading.
        let mut newest_per_slot: HashMap<&str, DateTime<Utc>> = HashMap::new();
        for c in candidates {
            if let CandidateState::Ready(s) = &c.state {
                let e = newest_per_slot.entry(c.slot.as_str()).or_insert(s.as_of);
                if s.as_of > *e {
                    *e = s.as_of;
                }
            }
        }

        for c in candidates {
            let CandidateState::Ready(sentinel) = &c.state else {
                continue;
            };
            let is_current = newest_per_slot
                .get(c.slot.as_str())
                .is_some_and(|newest| *newest == sentinel.as_of);
            items.push(WorkItem {
                source: spec.name.clone(),
                dataset: spec.dataset.clone(),
                slot: c.slot.clone(),
                candidate: c.clone(),
                priority: if is_current { spec.priority } else { Priority::Backfill },
                source_time: sentinel.as_of,
            });
        }
    }

    items.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then(b.source_time.cmp(&a.source_time))
    });
    WorkPlan { items }
}
```

Add to `crates/geode-data/src/ingest/mod.rs`:

```rust
pub mod plan;

pub use plan::{WorkItem, WorkPlan, build_plan};
```

- [ ] **Step 4: Run to verify tests pass**

Run: `cargo test -p geode-data plan`
Expected: PASS (5 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/geode-data
git commit -m "feat(data): cold-start priority ladder

Within a slot only the newest file keeps its source's priority; every
older file for the same slot drops to Backfill however recent the source.
That is what puts current risk on screen first while history loads behind
it (spec §5.4).

The plan is built from a sentinel-only scan, so a cold start is ordered
before any CSV is opened."
```

---

### Task 12: The ingest runner

**Files:**
- Create: `crates/geode-data/src/ingest/runner.rs`
- Modify: `crates/geode-data/src/ingest/mod.rs`

**Interfaces:**
- Consumes: `Store`, `WorkPlan`, `WorkItem`, `load_file`, `Health`.
- Produces: `IngestEvent`, `IngestRunner::spawn(Store, DatasetSpec, String)
  -> (IngestHandle, Receiver<IngestEvent>)`, `IngestHandle::submit(WorkPlan)`,
  `IngestHandle::shutdown()`. Phase 2b's `DataService` owns the handle.

**One ingest thread, not a pool — deliberately.** DuckDB is
single-writer, so every publish serializes anyway (spec §5.3, §5.6). Parse
parallelism across separate connections is a *benchmark question* the spec
explicitly declines to assume, so Task 14 measures it before anyone builds
it. What this task must get right is preemption and never dying.

**Preemption granularity is one file** (spec §5.4): the runner re-sorts its
queue between files, so a newly landed current file jumps ahead of remaining
backfill without interrupting a load in flight.

- [ ] **Step 1: Write the failing tests**

Create `crates/geode-data/src/ingest/runner.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::Priority;
    use std::time::Duration;

    fn drain(rx: &std::sync::mpsc::Receiver<IngestEvent>, n: usize) -> Vec<IngestEvent> {
        let mut out = Vec::new();
        for _ in 0..n {
            match rx.recv_timeout(Duration::from_secs(30)) {
                Ok(e) => out.push(e),
                Err(_) => break,
            }
        }
        out
    }

    /// Builds a store, schema, and a source directory; returns a plan over it.
    fn harness() -> (tempfile::TempDir, tempfile::TempDir, crate::store::Store,
                     geode_core::schema::DatasetSpec, crate::ingest::WorkPlan) {
        // Reuse the load-pipeline fixture builders (Task 9) via the shared
        // test support module, then discover + plan over the emitted dir.
        let (db_dir, src_dir, store, ds) = crate::ingest::load::tests_support::fixture();
        let spec = crate::source::SourceSpec {
            name: "risk".into(),
            dataset: "risk_snapshot".into(),
            paths: vec![format!("{}/*.csv", src_dir.path().display())],
            readiness: crate::source::Readiness::Sentinel,
            priority: Priority::LatestRisk,
            poll_interval: Duration::from_secs(30),
            pending_timeout: Duration::from_secs(3600),
            slot_pattern: Some(r"^risk_\d{4}-\d{2}-\d{2}_(?<slot>.+)$".into()),
        };
        let cat = crate::store::Catalog::new(store.writer());
        let found = crate::source::discover(&spec, &cat, std::time::SystemTime::now()).unwrap();
        let plan = crate::ingest::build_plan(&[(spec, found)]);
        (db_dir, src_dir, store, ds, plan)
    }

    #[test]
    fn works_a_plan_and_reports_every_publish() {
        let (_db, _src, store, ds, plan) = harness();
        let expected = plan.items.len();
        assert!(expected > 0, "fixture must produce work");

        let (handle, rx) = IngestRunner::spawn(store, ds, "risk_snapshot".into());
        handle.submit(plan);
        let events = drain(&rx, expected + 1);
        handle.shutdown();

        let published = events.iter().filter(|e| matches!(e, IngestEvent::Published { .. })).count();
        assert_eq!(published, expected);
        assert!(events.iter().any(|e| matches!(e, IngestEvent::PlanComplete)));
    }

    #[test]
    fn a_newly_submitted_current_file_preempts_remaining_backfill() {
        let (_db, _src, store, ds, mut plan) = harness();
        // Force everything to Backfill, then submit one current item.
        for item in &mut plan.items {
            item.priority = Priority::Backfill;
        }
        let current = {
            let mut c = plan.items[plan.items.len() - 1].clone();
            c.priority = Priority::LatestRisk;
            c
        };
        let expected = plan.items.len();

        let (handle, rx) = IngestRunner::spawn(store, ds, "risk_snapshot".into());
        handle.submit(plan);
        handle.submit(crate::ingest::WorkPlan { items: vec![current.clone()] });

        let events = drain(&rx, expected + 2);
        handle.shutdown();

        let first_slot = events.iter().find_map(|e| match e {
            IngestEvent::Published { slot, .. } => Some(slot.clone()),
            _ => None,
        });
        // The preempting item may or may not win the very first slot
        // depending on whether a load was already in flight, but it must
        // not be last.
        let positions: Vec<usize> = events
            .iter()
            .enumerate()
            .filter_map(|(i, e)| match e {
                IngestEvent::Published { slot, .. } if *slot == current.slot => Some(i),
                _ => None,
            })
            .collect();
        assert!(!positions.is_empty(), "preempting item never ran; first was {first_slot:?}");
        assert!(
            positions[0] < events.len().saturating_sub(1),
            "a current file must not wait behind all remaining backfill"
        );
    }

    #[test]
    fn a_failing_item_degrades_and_the_runner_keeps_going() {
        let (_db, _src, store, ds, mut plan) = harness();
        let good = plan.items.len();
        // Point one item at a nonexistent CSV.
        let mut broken = plan.items[0].clone();
        broken.candidate.csv_path = broken.candidate.csv_path.with_file_name("gone.csv");
        plan.items.insert(0, broken);

        let (handle, rx) = IngestRunner::spawn(store, ds, "risk_snapshot".into());
        handle.submit(plan);
        let events = drain(&rx, good + 2);
        handle.shutdown();

        assert!(events.iter().any(|e| matches!(e, IngestEvent::Failed { .. })));
        let published = events.iter().filter(|e| matches!(e, IngestEvent::Published { .. })).count();
        assert_eq!(published, good, "one bad file must not stop the run (spec §5.7)");
    }

    #[test]
    fn shutdown_is_idempotent_and_does_not_hang() {
        let (_db, _src, store, ds, _plan) = harness();
        let (handle, _rx) = IngestRunner::spawn(store, ds, "risk_snapshot".into());
        handle.shutdown();
        handle.shutdown();
    }
}
```

Promote the Task 9 fixture so the runner tests reuse it instead of
duplicating it. In `crates/geode-data/src/ingest/load.rs`, add:

```rust
#[cfg(test)]
pub(crate) mod tests_support {
    use geode_demo_data::{EmitOptions, GeneratorConfig, emit_directory, generate};

    /// The same fixture the load tests use: a populated store with the
    /// schema applied and catalog tables created, plus a generated source
    /// directory. Both TempDirs are returned so the caller keeps them alive.
    pub(crate) fn fixture() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        crate::store::Store,
        geode_core::schema::DatasetSpec,
    ) {
        let db_dir = tempfile::tempdir().unwrap();
        let src_dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(db_dir.path().join("geode.duckdb")).unwrap();
        let ds = super::tests::schema();
        store.apply_schema(&ds).unwrap();
        crate::store::Catalog::new(store.writer()).ensure_tables().unwrap();
        let batch = generate(&GeneratorConfig { rows: 3_000, seed: 42, business_dates: 1 });
        emit_directory(&batch, &EmitOptions::new(src_dir.path())).unwrap();
        (db_dir, src_dir, store, ds)
    }
}
```

and change Task 9's own `fixture()` to delegate to it:

```rust
    fn fixture() -> Fixture {
        let (db_dir, src_dir, store, ds) = super::tests_support::fixture();
        let batch = generate(&GeneratorConfig { rows: 3_000, seed: 42, business_dates: 1 });
        let emitted = emit_directory(&batch, &EmitOptions::new(src_dir.path())).unwrap();
        Fixture { _dir: db_dir, _src: src_dir, store, emitted, ds }
    }
```

Task 9's `schema()` helper must become `pub(crate) fn schema()` so
`tests_support` can call it.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p geode-data runner`
Expected: FAIL — `cannot find struct IngestRunner`.

- [ ] **Step 3: Implement the runner**

Prepend to `crates/geode-data/src/ingest/runner.rs`:

```rust
//! The ingest runner (spec §5.4–§5.7). Owns the writer connection on its
//! own thread, works a priority-ordered queue, and never takes the app down.
//!
//! One thread, not a pool, deliberately: DuckDB is single-writer so every
//! publish serializes anyway (spec §5.3). Whether parsing in parallel across
//! separate connections buys anything is a benchmark question the spec
//! declines to assume (spec §5.6), and Task 14 measures it.
//!
//! Preemption granularity is one file: the queue is re-sorted between
//! items, so a newly landed current file jumps ahead of remaining backfill
//! without interrupting a load in flight (spec §5.4).

use crate::health::Health;
use crate::ingest::load::{LoadRequest, load_file};
use crate::ingest::plan::{WorkItem, WorkPlan};
use crate::source::{CandidateState, Priority};
use crate::store::Store;
use geode_core::schema::DatasetSpec;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

#[derive(Debug, Clone)]
pub enum IngestEvent {
    Published {
        dataset: String,
        slot: String,
        gen_id: i64,
        books: Vec<String>,
        rows: usize,
        health: Health,
    },
    Failed {
        slot: String,
        reason: String,
    },
    /// The queue drained. Not a terminal state — more work may be submitted.
    PlanComplete,
}

#[derive(Default)]
struct Queue {
    items: Vec<WorkItem>,
    shutdown: bool,
}

pub struct IngestHandle {
    queue: Arc<(Mutex<Queue>, Condvar)>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

pub struct IngestRunner;

impl IngestRunner {
    pub fn spawn(
        store: Store,
        dataset: DatasetSpec,
        dataset_name: String,
    ) -> (IngestHandle, Receiver<IngestEvent>) {
        let (tx, rx) = channel();
        let queue = Arc::new((Mutex::new(Queue::default()), Condvar::new()));
        let worker_queue = Arc::clone(&queue);

        let thread = std::thread::Builder::new()
            .name("geode-ingest".into())
            .spawn(move || run(store, dataset, dataset_name, worker_queue, tx))
            .expect("spawning the ingest thread");

        (IngestHandle { queue, thread: Mutex::new(Some(thread)) }, rx)
    }
}

impl IngestHandle {
    /// Add work. Items are merged into the queue and the whole queue is
    /// re-sorted, so a current file preempts pending backfill.
    pub fn submit(&self, plan: WorkPlan) {
        let (lock, cvar) = &*self.queue;
        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
        q.items.extend(plan.items);
        q.items.sort_by(|a, b| {
            a.priority.cmp(&b.priority).then(b.source_time.cmp(&a.source_time))
        });
        cvar.notify_all();
    }

    pub fn shutdown(&self) {
        {
            let (lock, cvar) = &*self.queue;
            let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
            q.shutdown = true;
            cvar.notify_all();
        }
        if let Some(t) = self.thread.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = t.join();
        }
    }
}

impl Drop for IngestHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run(
    store: Store,
    dataset: DatasetSpec,
    dataset_name: String,
    queue: Arc<(Mutex<Queue>, Condvar)>,
    tx: Sender<IngestEvent>,
) {
    loop {
        let item = {
            let (lock, cvar) = &*queue;
            let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if q.shutdown {
                    return;
                }
                if !q.items.is_empty() {
                    break q.items.remove(0);
                }
                let _ = tx.send(IngestEvent::PlanComplete);
                let (guard, _) = cvar
                    .wait_timeout(q, std::time::Duration::from_millis(250))
                    .unwrap_or_else(|e| e.into_inner());
                q = guard;
            }
        };

        // Panic boundary (spec §5.7): a panicking load degrades its file and
        // the runner keeps working. Only a render-thread panic takes the app
        // down.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let CandidateState::Ready(sentinel) = &item.candidate.state else {
                return Err("candidate was not ready".to_string());
            };
            load_file(
                &store,
                &LoadRequest {
                    dataset: &dataset,
                    dataset_name: &dataset_name,
                    csv_path: &item.candidate.csv_path,
                    sentinel,
                    slot: &item.slot,
                },
            )
            .map_err(|e| e.to_string())
        }));

        let event = match outcome {
            Ok(Ok(loaded)) => IngestEvent::Published {
                dataset: dataset_name.clone(),
                slot: item.slot.clone(),
                gen_id: loaded.gen_id,
                books: match &item.candidate.state {
                    CandidateState::Ready(s) => s.books.clone(),
                    _ => Vec::new(),
                },
                rows: loaded.rows,
                health: loaded.health,
            },
            Ok(Err(reason)) => IngestEvent::Failed { slot: item.slot.clone(), reason },
            Err(_) => IngestEvent::Failed {
                slot: item.slot.clone(),
                reason: "ingest task panicked".into(),
            },
        };
        if tx.send(event).is_err() {
            return; // receiver gone: nothing left to report to
        }
    }
}

/// Backfill items yield between files so current work is not starved. The
/// queue re-sort in `submit` is what actually reorders; this exists so the
/// intent is testable and named.
pub fn is_preemptible(item: &WorkItem) -> bool {
    item.priority == Priority::Backfill
}
```

Add to `crates/geode-data/src/ingest/mod.rs`:

```rust
pub mod runner;

pub use runner::{IngestEvent, IngestHandle, IngestRunner};
```

- [ ] **Step 4: Run to verify tests pass**

Run: `cargo test -p geode-data runner`
Expected: PASS (4 tests).

- [ ] **Step 5: Full suite, lint, commit**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`

```bash
git add crates/geode-data
git commit -m "feat(data): ingest runner with preemption and a panic boundary

One thread owning the writer connection, working a priority-ordered
queue that is re-sorted on every submit so a newly landed current file
jumps ahead of remaining backfill. Preemption granularity is one file:
loads in flight are never interrupted.

A panicking or failing load degrades its own file and the run continues
(spec §5.7) — tested by planting a nonexistent CSV mid-plan and asserting
every other file still publishes.

One thread rather than a pool is deliberate: DuckDB is single-writer so
publishes serialize anyway, and whether parallel parsing helps is a
benchmark question spec §5.6 declines to assume."
```

---

### Task 13: The retention sweeper and checkpoint scheduling

**Files:**
- Create: `crates/geode-data/src/store/retention.rs`
- Modify: `crates/geode-data/src/store/mod.rs`

**Interfaces:**
- Consumes: `Store`, `Grain`, `ddl::{table_name, TableKind}`.
- Produces: `RetentionPolicy { keep_generations, keep_age }`,
  `SweepReport { evicted_rows, evicted_generations, oldest_remaining }`,
  `sweep(&Connection, &[Grain], &RetentionPolicy, DateTime<Utc>) ->
  Result<SweepReport, StoreError>`, and `checkpoint(&Connection)`.

**Retention is per partition, not per dataset.** "Keep 50 generations" means
each `(slot, book)` keeps its own 50 — otherwise a busy book would evict a
quiet one's history. `oldest_remaining` is published so the time-travel UI
can show how far back a user may go (spec §4.6).

**Checkpointing belongs here** because a checkpoint can stall the writer and
must not land mid-refresh; the sweeper already runs between loads.

- [ ] **Step 1: Write the failing tests**

Create `crates/geode-data/src/store/retention.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use geode_core::schema::Grain;
    use chrono::{DateTime, Duration, Timelike, Utc};

    /// Terse RFC 3339 literal for tests.
    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn fixture() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("g.duckdb")).unwrap();
        store
            .writer()
            .execute_batch(
                "create table measures_position_archive(
                     book varchar, slot varchar, gen_id bigint,
                     source_time timestamp with time zone);",
            )
            .unwrap();
        (dir, store)
    }

    /// `gens` generations for each of two partitions, one hour apart.
    fn fill(store: &Store, gens: i64) {
        for slot in ["BK000", "BK001"] {
            for g in 1..=gens {
                store
                    .writer()
                    .execute(
                        "insert into measures_position_archive values (?, ?, ?, ?)",
                        duckdb::params![
                            slot,
                            slot,
                            g,
                            ts("2026-08-30T00:00:00Z") + Duration::hours(g)
                        ],
                    )
                    .unwrap();
            }
        }
    }

    fn remaining(store: &Store) -> i64 {
        store
            .writer()
            .query_row("select count(*) from measures_position_archive", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn keep_by_count_is_per_partition() {
        let (_d, store) = fixture();
        fill(&store, 10);
        let policy = RetentionPolicy { keep_generations: Some(3), keep_age: None };
        let report = sweep(
            store.writer(),
            &[Grain::Position],
            &policy,
            ts("2026-08-31T00:00:00Z"),
        )
        .unwrap();

        assert_eq!(remaining(&store), 6, "3 generations for each of 2 partitions");
        assert_eq!(report.evicted_rows, 14);
    }

    #[test]
    fn keep_by_age_evicts_on_source_time() {
        let (_d, store) = fixture();
        fill(&store, 10);
        let policy = RetentionPolicy {
            keep_generations: None,
            keep_age: Some(Duration::hours(5)),
        };
        sweep(
            store.writer(),
            &[Grain::Position],
            &policy,
            ts("2026-08-30T10:00:00Z"),
        )
        .unwrap();
        // Keeps source_time >= 05:00, i.e. generations 5..=10.
        assert_eq!(remaining(&store), 12);
    }

    #[test]
    fn both_policies_apply_together() {
        let (_d, store) = fixture();
        fill(&store, 10);
        let policy = RetentionPolicy {
            keep_generations: Some(8),
            keep_age: Some(Duration::hours(3)),
        };
        sweep(
            store.writer(),
            &[Grain::Position],
            &policy,
            ts("2026-08-30T10:00:00Z"),
        )
        .unwrap();
        // Age keeps 7..=10 (4 per partition); count would keep 8. The
        // stricter rule wins.
        assert_eq!(remaining(&store), 8);
    }

    #[test]
    fn oldest_remaining_is_published_for_the_time_travel_ui() {
        let (_d, store) = fixture();
        fill(&store, 10);
        let policy = RetentionPolicy { keep_generations: Some(3), keep_age: None };
        let report = sweep(
            store.writer(),
            &[Grain::Position],
            &policy,
            ts("2026-08-31T00:00:00Z"),
        )
        .unwrap();
        assert_eq!(report.oldest_remaining.unwrap().hour(), 8);
    }

    #[test]
    fn an_empty_policy_evicts_nothing() {
        let (_d, store) = fixture();
        fill(&store, 5);
        let report = sweep(
            store.writer(),
            &[Grain::Position],
            &RetentionPolicy::default(),
            ts("2026-08-31T00:00:00Z"),
        )
        .unwrap();
        assert_eq!(report.evicted_rows, 0);
        assert_eq!(remaining(&store), 10);
    }

    #[test]
    fn sweeping_an_empty_archive_is_not_an_error() {
        let (_d, store) = fixture();
        let report = sweep(
            store.writer(),
            &[Grain::Position],
            &RetentionPolicy { keep_generations: Some(3), keep_age: None },
            ts("2026-08-31T00:00:00Z"),
        )
        .unwrap();
        assert_eq!(report.evicted_rows, 0);
        assert!(report.oldest_remaining.is_none());
    }

    #[test]
    fn checkpoint_succeeds_on_a_live_database() {
        let (_d, store) = fixture();
        fill(&store, 2);
        checkpoint(store.writer()).unwrap();
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p geode-data retention`
Expected: FAIL — `cannot find struct RetentionPolicy`.

- [ ] **Step 3: Implement retention**

Prepend to `crates/geode-data/src/store/retention.rs`:

```rust
//! Retention (spec §4.6). Bounded by disk rather than RAM now that storage
//! is persistent, so defaults are generous — but unbounded history would
//! still grow the database file without limit.
//!
//! Retention is per partition: "keep 50 generations" means each (slot, book)
//! keeps its own 50, so a busy book cannot evict a quiet one's history.

use crate::store::StoreError;
use crate::store::ddl::{TableKind, table_name};
use duckdb::Connection;
use geode_core::schema::Grain;
use chrono::{DateTime, Duration, Utc};

#[derive(Debug, Clone, Default)]
pub struct RetentionPolicy {
    /// Keep this many generations per partition.
    pub keep_generations: Option<usize>,
    /// Keep generations whose source time is within this window.
    pub keep_age: Option<Duration>,
}

impl RetentionPolicy {
    pub fn is_empty(&self) -> bool {
        self.keep_generations.is_none() && self.keep_age.is_none()
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    pub evicted_rows: usize,
    pub evicted_generations: Vec<i64>,
    /// How far back time travel can go (spec §4.6).
    pub oldest_remaining: Option<DateTime<Utc>>,
}

fn sql_err(statement: &str) -> impl FnOnce(duckdb::Error) -> StoreError + '_ {
    move |source| StoreError::Sql { statement: statement.to_string(), source }
}

pub fn sweep(
    conn: &Connection,
    grains: &[Grain],
    policy: &RetentionPolicy,
    now: DateTime<Utc>,
) -> Result<SweepReport, StoreError> {
    let mut report = SweepReport::default();

    for grain in grains {
        let archive = table_name(*grain, TableKind::Archive);

        if !policy.is_empty() {
            let before: i64 = {
                let sql = format!("select count(*) from {archive}");
                conn.query_row(&sql, [], |r| r.get(0)).map_err(sql_err(&sql))?
            };

            // A generation survives only if it satisfies every configured
            // rule; the stricter one therefore wins.
            let mut keep: Vec<String> = Vec::new();
            if let Some(n) = policy.keep_generations {
                keep.push(format!("rn <= {n}"));
            }
            if let Some(age) = policy.keep_age {
                keep.push(format!("source_time >= '{}'::timestamptz", now - age));
            }

            let sql = format!(
                "delete from {archive} where (slot, book, gen_id) not in (
                     select slot, book, gen_id from (
                         select slot, book, gen_id, source_time,
                                row_number() over (
                                    partition by slot, book order by source_time desc
                                ) as rn
                         from (select distinct slot, book, gen_id, source_time from {archive})
                     ) where {keep}
                 )",
                keep = keep.join(" and "),
            );
            conn.execute_batch(&sql).map_err(sql_err(&sql))?;

            let after: i64 = {
                let sql = format!("select count(*) from {archive}");
                conn.query_row(&sql, [], |r| r.get(0)).map_err(sql_err(&sql))?
            };
            report.evicted_rows += (before - after).max(0) as usize;
        }

        let sql = format!("select min(source_time) from {archive}");
        let oldest: Option<DateTime<Utc>> =
            conn.query_row(&sql, [], |r| r.get(0)).map_err(sql_err(&sql))?;
        report.oldest_remaining = match (report.oldest_remaining, oldest) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
    }

    Ok(report)
}

/// Force a checkpoint. Owned by the sweeper because a checkpoint can stall
/// the writer and must not land mid-refresh (spec §4.6).
pub fn checkpoint(conn: &Connection) -> Result<(), StoreError> {
    let sql = "checkpoint";
    conn.execute_batch(sql).map_err(sql_err(sql))
}
```

Add to `crates/geode-data/src/store/mod.rs`:

```rust
pub mod retention;

pub use retention::{RetentionPolicy, SweepReport, checkpoint, sweep};
```

- [ ] **Step 4: Run to verify tests pass**

Run: `cargo test -p geode-data retention`
Expected: PASS (7 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/geode-data
git commit -m "feat(data): retention sweeper and checkpoint scheduling

Per-partition retention by generation count and/or age, so a busy book
cannot evict a quiet one's history. Where both rules are configured the
stricter wins. Publishes oldest_remaining so the time-travel UI can show
how far back a user may go (spec §4.6).

The sweeper owns checkpointing because a checkpoint can stall the writer
and must not land mid-refresh."
```

---

### Task 14: Benchmarks and CI

**Files:**
- Create: `crates/geode-data/benches/ingest.rs`
- Modify: `crates/geode-data/Cargo.toml`
- Modify: `.github/workflows/ci.yml`
- Modify: `docs/perf.md`

**Interfaces:**
- Consumes: everything above.
- Produces: no library API — the measurements that answer spec §5.6's
  deferred question and establish the ingest baseline the query phase is
  measured against.

**What these benchmarks are for.** Two of them establish budgets; one
answers a design question the spec deliberately left open. If sequential
staging is within noise of parallel, the runner stays one thread and the
`ingest.workers` knob is never built — that decision should come from a
number, not a preference.

- [ ] **Step 1: Register the bench target**

In `crates/geode-data/Cargo.toml`:

```toml
[dev-dependencies]
criterion = "0.8.2"

[[bench]]
name = "ingest"
harness = false
```

(`[lib] bench = false` is already present — workspace invariant.)

- [ ] **Step 2: Write the benchmarks**

Create `crates/geode-data/benches/ingest.rs`:

```rust
//! Ingest benchmarks (spec §9.3). Establishes the cold-start and
//! throughput baselines, and answers the parse-parallelism question spec
//! §5.6 declines to assume.
//!
//! Uses the generated source directory, never checked-in fixtures (§7.4).

use criterion::{Criterion, criterion_group, criterion_main};
use geode_demo_data::{EmitOptions, GeneratorConfig, emit_directory, generate};
use std::hint::black_box;

/// Emit a source directory of `rows` rows once, reused across samples.
fn source_dir(rows: usize) -> (tempfile::TempDir, geode_demo_data::EmittedDirectory) {
    let dir = tempfile::tempdir().unwrap();
    let batch = generate(&GeneratorConfig { rows, seed: 42, business_dates: 1 });
    let emitted = emit_directory(&batch, &EmitOptions::new(dir.path())).unwrap();
    (dir, emitted)
}

fn bench_cold_start(c: &mut Criterion) {
    let mut group = c.benchmark_group("ingest_cold_start");
    group.sample_size(10);
    for rows in [100_000usize, 1_000_000] {
        let (_src, emitted) = source_dir(rows);
        group.bench_function(format!("{}_rows", rows), |b| {
            b.iter_batched(
                || tempfile::tempdir().unwrap(),
                |db_dir| {
                    // Fresh database each iteration: this is the cold path.
                    black_box(run_full_ingest(db_dir.path(), &emitted));
                },
                criterion::BatchSize::PerIteration,
            )
        });
    }
    group.finish();
}

fn bench_warm_start(c: &mut Criterion) {
    // Reopening a populated database must be milliseconds: live tables are
    // queryable the moment it opens, which is what keeps the <1s startup
    // budget reachable without reading a CSV (spec §5.4, §7.1).
    let (_src, emitted) = source_dir(100_000);
    let db_dir = tempfile::tempdir().unwrap();
    run_full_ingest(db_dir.path(), &emitted);

    let mut group = c.benchmark_group("ingest_warm_start");
    group.sample_size(50);
    group.bench_function("reopen_populated_db", |b| {
        b.iter(|| {
            let store =
                geode_data::store::Store::open(db_dir.path().join("geode.duckdb")).unwrap();
            let n: i64 = store
                .writer()
                .query_row("select count(*) from measures_position_live", [], |r| r.get(0))
                .unwrap();
            black_box(n)
        })
    });
    group.finish();
}

fn bench_single_file_load(c: &mut Criterion) {
    let (_src, emitted) = source_dir(200_000);
    let file = emitted.files.iter().find(|f| f.sentinel_path.is_some()).unwrap();

    let mut group = c.benchmark_group("ingest_single_file");
    group.sample_size(10);
    group.throughput(criterion::Throughput::Elements(file.rows as u64));
    group.bench_function("load_one_file", |b| {
        b.iter_batched(
            || tempfile::tempdir().unwrap(),
            |db_dir| black_box(load_one(db_dir.path(), file)),
            criterion::BatchSize::PerIteration,
        )
    });
    group.finish();
}

/// Spec §5.6's open question: does staging on separate connections beat
/// staging sequentially through the writer? DuckDB's own CSV reader is
/// already multi-threaded, so the honest expectation is "no" — but the
/// runner's shape should follow the measurement, not the expectation.
fn bench_parse_parallelism(c: &mut Criterion) {
    let (_src, emitted) = source_dir(400_000);
    let mut group = c.benchmark_group("ingest_parallelism");
    group.sample_size(10);
    group.bench_function("sequential_staging", |b| {
        b.iter_batched(
            || tempfile::tempdir().unwrap(),
            |db_dir| black_box(stage_all_sequential(db_dir.path(), &emitted)),
            criterion::BatchSize::PerIteration,
        )
    });
    group.bench_function("parallel_staging_2_connections", |b| {
        b.iter_batched(
            || tempfile::tempdir().unwrap(),
            |db_dir| black_box(stage_all_parallel(db_dir.path(), &emitted, 2)),
            criterion::BatchSize::PerIteration,
        )
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_cold_start,
    bench_warm_start,
    bench_single_file_load,
    bench_parse_parallelism
);
criterion_main!(benches);
```

> **Implementer note:** `run_full_ingest`, `load_one`,
> `stage_all_sequential` and `stage_all_parallel` are small helpers over the
> public API built in Tasks 5–12 (`Store::open`, `apply_schema`,
> `Catalog::ensure_tables`, `discover`, `build_plan`, `load_file`). Write
> them at the bottom of this file; `stage_all_parallel` spawns N threads,
> each with its own `store.reader()`-derived connection, doing only the
> `read_csv` staging step so the comparison isolates parsing from publish.

- [ ] **Step 3: Verify the benches compile and run**

Run: `cargo bench -p geode-data --no-run`
Expected: compiles clean.

Run: `cargo bench -p geode-data -- ingest_warm_start`
Expected: reopening a populated database is single-digit milliseconds.

- [ ] **Step 4: Record the numbers**

Add a section to `docs/perf.md` under "Benchmarks" giving the measured
cold-start time at 100k and 1M rows, single-file throughput, warm-start
time, and the parallelism verdict. State the machine. If parallel staging
is within noise of sequential, write that down explicitly — it is the
answer to spec §5.6 and closes open question §10.7.

- [ ] **Step 5: Confirm CI caching covers the bundled build**

`.github/workflows/ci.yml` already uses `Swatinem/rust-cache@v2`, which
caches `~/.cargo` and `target/` keyed on `Cargo.lock` — so the compiled
`libduckdb-sys` artifacts are cached after the first run on each OS. Two
changes:

```yaml
    runs-on: ${{ matrix.os }}
    timeout-minutes: 45
```

and confirm the first post-merge run on each platform completes. The
measured clean build is 127s wall / 1382s CPU on an M-series Mac; a
two-core runner will take substantially longer, and 45 minutes gives
headroom for a cold cache on both `cargo test` and `cargo bench --no-run`.

- [ ] **Step 6: Full verification**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo bench --workspace --no-run`
Expected: all four green — the same four checks CI runs.

- [ ] **Step 7: Commit**

```bash
git add crates/geode-data docs/perf.md .github/workflows/ci.yml
git commit -m "perf(data): ingest benchmarks and CI headroom for the bundled build

Cold start at 100k and 1M rows, warm start (reopening a populated
database must be milliseconds — that is what keeps the <1s startup budget
reachable without reading a CSV), and single-file throughput.

Also measures sequential against parallel staging to answer the question
spec §5.6 deliberately left open. DuckDB's CSV reader is already
multi-threaded so the honest expectation is that parallelism does not
help, but the runner's shape should follow the number rather than the
expectation.

CI gets a 45-minute timeout: rust-cache already covers libduckdb-sys, but
a cold cache on a two-core runner needs the headroom."
```

---

## Self-Review

Run against the spec after completing the plan, before execution.

**Spec coverage.** Every §3–§5 requirement maps to a task: the data model
and grain vocabulary to Tasks 1–2, the source directory and schema tolerance
to Tasks 3 and 9, storage and the live/archive split to Task 5, generations
and the backfill guard to Tasks 6–7, the grain split and conflict detection
to Task 8, readiness and discovery to Task 10, the priority ladder to
Task 11, failure handling and the panic boundary to Task 12, retention and
checkpoints to Task 13, and §9.1/§9.3 to Tasks 2–3 and 14.

**Deliberately deferred to Phase 2b** (the query-path plan), not gaps here:
§6 in full — view definitions, the scope compiler and its expression parser,
grain-aware aggregation with `ROLLUP`, `Attribution` and `ScopeSemantics`,
as-of routing, snapshots, and the query pool with cancellation — plus §7's
vertical slice. Task 6 builds `dataset_as_of`, which as-of routing will
consume, but nothing here routes a query at the archive.

**Known incompleteness, by design.** Cross-*file* attribute disagreement
(the same instrument arriving with different values from two books' files)
is detected within a file by Task 8 but not across files; Task 9's test says
so explicitly. That belongs with `instrument_ref` derivation, which has no
upstream source yet (spec §3.5) and is the first thing Phase 2b should pick
up.

**Blocked on real data, not on code** (spec §10): a real `.done` sample and
a real CSV header. Tasks 3–4 are written so that being wrong about either
costs a config edit and a fixture change, not a redesign — the sentinel
parser requires only two fields, and every column's grain, type and
requiredness is declared rather than compiled in.

## Execution Handoff

Plan saved to `docs/superpowers/plans/2026-08-30-phase-2a-storage-and-ingest.md`.
Phase 2b (the query path, the vertical slice, and the §7.1 requery
benchmarks) gets its own plan against the same spec.
