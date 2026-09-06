# Phase 4a — Frame Features Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Put a face on the frame: carried dimensions and the categorical
flag in the schema, a scope bar with chips and a per-keystroke text
filter made affordable by matching ENUM dictionaries, dimension pickers
over a new distinct-values request, an as-of selector with an unmissable
historical indicator, the tile-local `:filter` layer, scope undo/redo,
saved scopes, and the atomic flip across tiles.

**Architecture:** `geode-core` grows the schema vocabulary (carried
dimensions, `categorical`), `ScopeSpec`, an `Expr` renderer, and the
`Distinct` request/outcome types both ends share. `geode-data` reads the
new vocabulary at every grain-aware site (split, DDL, routing, tree
compiler, attribution, interning), adds `Request::Distinct` on the pool,
and rewrites the text filter's ENUM terms. `geode-shell` replaces the
readout with a cached scope-bar model, makes the toolbar's field live,
adds the picker and as-of modals in the keybinding dialog's mould, grows
`Frame` (undo/redo stacks, previous as-of, recent publishes, saved
scopes, the flip barrier), and persists `[frame]` in the session.
`geode-blotter` gains `:filter`, the new `:scope`/`:asof` forms, and
snapshot staging for the flip. `geode-app` registers the per-column pick
actions, routes `Distinct` both ways, and feeds `Published` details to
the frame.

**Tech Stack:** Rust 2024, gpui (`uniform_list` for the values list),
gpui-component `0e2fb7a` (`Input`, `InputEvent`), DuckDB 1.10505
(`enum_range`, `any_value`), `toml_edit`, `chrono`, `criterion`.

**Spec:** `docs/superpowers/specs/2026-09-06-geode-phase-4-frame-features-design.md`
§1.1 (4a), §1.2, §2, §3 (all of it), §7, §8, §10 (4a steps 1–9).
Phases 3a–3c are prerequisites and are consumed as-is.

## Global Constraints

- **Layering:** `shell` and `data` never depend on each other; both
  depend on `core`. `geode-app` is the only place they meet. No crate
  but `geode-data` opens a socket; config and session writes happen in
  `geode-shell` only, through `theme::write_atomic` (the write door is
  generalised in 4c, not here).
- **CI:** `cargo fmt --check`, `cargo clippy --workspace --all-targets
  -- -D warnings`, `cargo test --workspace`, `cargo bench --workspace
  --no-run`, and `cargo check -p geode-shell --features test-support
  --all-targets`, on macOS and Windows. Every task ends green.
- **Per-frame heap churn is a defect.** The scope-bar model, like the
  readout it replaces, is cached on `Frame::versions()`. Chips, pickers
  and the as-of modal rebuild only on a version or keystroke.
- **Nothing stalls the render thread.** Every config write happens on
  the background executor via a `take_pending_*` drain from the frame's
  observer, exactly as `persist_slot_to_user_config` does today.
- **The read path's opinions are law.** A carried dimension's attribution
  falls out of `attribution_of`; nothing in the blotter special-cases
  it.
- **Mutation harness:** commit before you mutate; every behaviour a task
  changes gets an entry with the 6th-argument test filter; run
  `zsh scripts/mutation-check.sh --changed` at the end of every task;
  one unfiltered run at the end of the branch, started detached
  (`pgrep -f "^zsh scripts/mutation-check.sh"` to watch it, never a bare
  `pgrep -f`).
- **Demo database:** `examples/demo-config/datasets.toml`'s header
  warns that `apply_schema` is `CREATE TABLE IF NOT EXISTS`. Task 1
  changes columns, so any persisted demo database must be deleted before
  the next `--demo` run; the task records where it lives.
- **Every new target carries `bench = false`;** no new targets are
  expected in 4a.
- Commit trailers on every commit:
  ```
  Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_013f4ftJp6GLNLTj3EBs7XFL
  ```

## What already exists (do not rebuild)

- `geode_core::scope::{Scope, DimensionSelection, Expr, parse_expr,
  CompareOp, Literal}`; `Scope::and_then`, `Scope::columns`,
  `Scope::validate`, `Scope::impossible`.
- `geode_core::schema::{DatasetSpec, SchemaSpec, ColumnSpec, ColumnRole,
  Grain}`; `Grain::{key_columns, dimension_key_columns,
  identity_columns, ALL}`; `SchemaSpec::from_doc`.
- `geode_core::groupings::GroupingSlots` and its `from_doc` — the model
  for `ScopeSpec::from_doc`.
- `geode_core::query::{AsOf, QueryKey, QueryOutcome}`.
- `geode_core::attribution::attribution_of`.
- `geode_data::handle::{DataHandle, Request}`, `DataService::{query,
  cancel, replace_views}`, `QueryPool::{submit, cancel}`, `run_one`,
  `DataEvent`, `EventSink`.
- `geode_data::query::scope_sql::{compile_scope, Era, evaluable_at,
  route, membership, like_pattern}`; `compile::compile_view`.
- `geode_data::ingest::split::{split_by_grain, payload_columns,
  key_exprs, Conflict}`; `store::ddl::{create_table_sql,
  dimension_columns, refresh_enum, enum_type_name, table_name}`.
- `geode_shell::frame::{Frame, FrameVersions, FrameReadout,
  persist_slot_to_user_config}` (readout is replaced in Task 3).
- `geode_shell::shell::dialog::{open_shell_dialog_with_key,
  ModalKeyHandler, filter_row, init_reclaimed_keybindings}`;
  `keybindings_view::open` as the keyed-modal pattern; `ShellView::
  {dialog_input, filter_input, modal, keybindings, settings}`.
- `geode_shell::palette::{fuzzy_match, PaletteState, PaletteItem,
  render, VISIBLE_ROWS, ROW_HEIGHT}`.
- `geode_shell::defaults::{register_builtin_actions, BUILTIN_KEYMAP}`;
  `ShellView::dispatch` arms in `shell/input.rs`.
- `geode_shell::session::{to_toml, from_toml, to_string_pretty, save,
  load, TileRecords, Restored}`.
- `geode_blotter::core::commands::{Command, parse, completions,
  Vocabulary, parse_as_of}`; `BlotterTile::{command, deliver, requery,
  serialize, follows_changed}`.
- `geode-app/src/bridge.rs::attach` — the drain loop; `main.rs` roster
  and registry setup.
- `crates/geode-data/benches/query.rs` — the 1M-row requery bench.

## File Structure

| File | Responsibility after 4a |
|---|---|
| `crates/geode-core/src/schema/column.rs` | `ColumnRole::Dimension { grain: Option<Grain> }`, `ColumnSpec::categorical`, `carried_grain()` |
| `crates/geode-core/src/schema/mod.rs` | parse `grain`/`categorical` on dimensions; `dimensions_at`, `carried_dimensions_at`, `categorical_columns`, `carries`; load-time rules for bare dimensions and `textual` |
| `crates/geode-core/src/attribution.rs` | `attribution_of(ds, grain, grouping, dims)` |
| `crates/geode-core/src/scope/expr.rs` | `impl Display for Expr` (round-trips through `parse_expr`) |
| `crates/geode-core/src/scopes.rs` (new) | `ScopeSpec`: `saved_scopes_from_doc`, `scope_to_table` |
| `crates/geode-core/src/query.rs` | `DistinctParams`, `DistinctOutcome`, `parse_as_of` (moved from the blotter) |
| `crates/geode-data/src/store/ddl.rs` | `categorical_columns` replaces `dimension_columns`; carried dimensions in `create_table_sql`; `existing_enum_types` moves here |
| `crates/geode-data/src/ingest/split.rs` | carried dimensions in `payload_columns` |
| `crates/geode-data/src/ingest/load.rs` | interning by `categorical`; a carried-dimension conflict degrades health |
| `crates/geode-data/src/query/scope_sql.rs` | `evaluable_at`/`route` read `dimensions_at`; the ENUM text rewrite; `compile_scope` uses `conn` |
| `crates/geode-data/src/query/compile.rs` | `carries_all`/`derived_for`/`own` read `dimensions_at`; `attribution_of` takes `ds`; `era_for` extracted |
| `crates/geode-data/src/query/distinct.rs` (new) | `compile_distinct` → one `CompiledQuery` per column |
| `crates/geode-data/src/query/pool.rs` | `RequestKind` on `QueryRequest`/`QueryResult` |
| `crates/geode-data/src/service.rs` | `DataEvent::Distinct`, `DataService::distinct`, the sink maps by kind |
| `crates/geode-data/src/handle.rs` | `Request::Distinct`, `DataHandle::distinct` |
| `crates/geode-data/benches/query.rs` | permanent text-filter cases; the keys-textual reopen |
| `crates/geode-shell/src/frame.rs` | undo/redo stacks, `previous_as_of`, `recent_publishes`, saved scopes, `bar_model`, the flip barrier, `persist_scope_to_user_config` |
| `crates/geode-shell/src/scopebar.rs` (new) | pure `ScopeBarModel`/`Chip` and `build_model(&Frame)` |
| `crates/geode-shell/src/shell/toolbar.rs` | renders the model: chips with close glyphs, the live field, the as-of marker |
| `crates/geode-shell/src/shell/picker.rs` (new) | `PickerState` (pure) + `open`, `handle_key`, `build` |
| `crates/geode-shell/src/shell/asof_view.rs` (new) | `AsOfState` (pure) + `open`, `handle_key`, `build` |
| `crates/geode-shell/src/shell/mod.rs` | `ShellView::{picker, as_of_dialog, pickable, filter_session, last_flip_versions}`; `deliver_distinct`; `ShellEvent::DistinctRequested` |
| `crates/geode-shell/src/shell/input.rs` | new `frame::*` dispatch arms; the field's enter/escape |
| `crates/geode-shell/src/shell/render.rs` | the as-of stripe; toolbar call |
| `crates/geode-shell/src/shell/status.rs` | the as-of segment |
| `crates/geode-shell/src/shell/hot_reload.rs` | `scopes` reload → `replace_saved_scopes`; `pickable` rebuild |
| `crates/geode-shell/src/shell/session_io.rs` | `[frame]` in the flush |
| `crates/geode-shell/src/session.rs` | `FrameRecord` to/from TOML |
| `crates/geode-shell/src/defaults.rs` | new actions + bindings; `register_pick_actions` |
| `crates/geode-shell/src/shell/tests/{scopebar,picker,asof,flip}.rs` (new) | `TestAppContext` tests per seam |
| `crates/geode-blotter/src/core/commands.rs` | `:filter`, `:scope drop/redo/save/load`, `:asof undo` |
| `crates/geode-blotter/src/tile.rs` | tile filter + `filtered` pill + session; staged snapshot for the flip |
| `crates/geode-app/src/main.rs` | `register_pick_actions` before the keymap builds |
| `crates/geode-app/src/bridge.rs` | `DistinctRequested` → `handle.distinct`; `DataEvent::Distinct` → `deliver_distinct`; `Published` details to the frame |
| `examples/demo-config/datasets.toml` | three carried dimensions; `textual` on string columns |
| `scripts/mutation-check.sh` | one entry per behaviour |
| `docs/perf.md`, `CLAUDE.md` | the text-filter table; the Phase 4a paragraph |

---

### Task 1: Carried dimensions and the `categorical` flag

The grain vocabulary (spec §3.3, §2 items 10–11). This task touches
every site `CLAUDE.md` names as mutation-mandatory, so it stands alone
and is reviewed before anything is built on it.

**Files:**
- Modify: `crates/geode-core/src/schema/column.rs`
- Modify: `crates/geode-core/src/schema/mod.rs`
- Modify: `crates/geode-core/src/attribution.rs`
- Modify: `crates/geode-data/src/store/ddl.rs`
- Modify: `crates/geode-data/src/ingest/split.rs`
- Modify: `crates/geode-data/src/ingest/load.rs:240-262` (interning loop) and the `degradations` block near line 270
- Modify: `crates/geode-data/src/query/scope_sql.rs:87-128` (`evaluable_at`, `route`)
- Modify: `crates/geode-data/src/query/compile.rs:56-63, 239-249, 385-392, 532, 786`
- Modify: `examples/demo-config/datasets.toml`
- Modify: `scripts/mutation-check.sh`
- Test: in-file `mod tests` of each file above

**Interfaces:**
- Consumes: `Grain::{key_columns, dimension_key_columns, ALL}`,
  `ColumnSpec`, `DatasetSpec`, `attribution_of`.
- Produces:
  ```rust
  // geode_core::schema
  pub enum ColumnRole {
      Key,
      /// `None`: a built-in grain key column. `Some(g)`: carried by `g`
      /// and every finer grain (spec §3.3).
      Dimension { grain: Option<Grain> },
      Measure { grain: Grain, aggregate: Aggregate },
      Attribute { grain: Grain },
  }
  pub struct ColumnSpec { /* … */ pub categorical: bool, /* … */ }
  impl ColumnSpec {
      pub fn carried_grain(&self) -> Option<Grain>;   // Some for a carried dimension only
  }
  impl DatasetSpec {
      /// Whether `grain` carries `column` as a dimension: a dimension key
      /// of the grain, or a carried dimension whose declaring grain's key
      /// is contained in this grain's dimension key.
      pub fn carries(&self, grain: Grain, column: &str) -> bool;
      /// Dimension keys plus carried dimensions, names in schema order.
      pub fn dimensions_at(&self, grain: Grain) -> Vec<&str>;
      /// Carried dimensions only (payload columns), for the split and DDL.
      pub fn carried_dimensions_at(&self, grain: Grain) -> Vec<&ColumnSpec>;
      pub fn categorical_columns(&self) -> Vec<&str>;
  }
  // geode_core::attribution
  pub fn attribution_of(ds: &DatasetSpec, grain: Grain, grouping: &[String], dims: &DerivedDimensions) -> Attribution;
  // geode_data::store::ddl
  pub fn categorical_columns(ds: &DatasetSpec) -> Vec<&str>;   // replaces dimension_columns
  ```

- [ ] **Step 1: Write the failing schema tests**

Append to `crates/geode-core/src/schema/mod.rs`'s `mod tests`:

```rust
    const CARRIED: &str = r#"
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
[risk.columns.currency]
type = "utf8"
role = "dimension"
grain = "instrument"
[risk.columns.expiry]
type = "utf8"
role = "attribute"
grain = "instrument"
categorical = true
[risk.columns.npv]
type = "f64"
role = "measure"
grain = "position"
[risk.columns.delta01]
type = "f64"
role = "measure"
grain = "underlying"
"#;

    #[test]
    fn a_dimension_may_declare_the_grain_that_carries_it() {
        let (schema, diags) = SchemaSpec::from_doc(&doc(CARRIED));
        assert!(diags.is_empty(), "{diags:?}");
        let ds = schema.dataset("risk").unwrap();
        let currency = ds.column("currency").unwrap();
        assert_eq!(
            currency.role,
            ColumnRole::Dimension {
                grain: Some(Grain::Instrument)
            }
        );
        assert_eq!(currency.carried_grain(), Some(Grain::Instrument));
        assert_eq!(currency.grain(), None, "a carried dimension is not a payload-by-grain column");
        assert_eq!(ds.column("book").unwrap().carried_grain(), None);
    }

    #[test]
    fn a_carried_dimension_is_carried_by_its_grain_and_every_finer_one() {
        let (schema, _) = SchemaSpec::from_doc(&doc(CARRIED));
        let ds = schema.dataset("risk").unwrap();
        assert!(!ds.carries(Grain::Position, "currency"), "a position spans instruments");
        assert!(ds.carries(Grain::Instrument, "currency"));
        assert!(ds.carries(Grain::Underlying, "currency"));
        assert!(ds.carries(Grain::UnderlyingPair, "currency"));
        // Key dimensions are carried exactly where they were before.
        assert!(ds.carries(Grain::Position, "book"));
        assert!(!ds.carries(Grain::Position, "underlying_ref"));
        assert!(ds.carries(Grain::Underlying, "underlying_ref"));
        // dimensions_at is keys then carried, schema order.
        assert_eq!(
            ds.dimensions_at(Grain::Instrument),
            vec!["book", "lhu", "position_ref", "counterparty", "instrument_ref", "currency"]
        );
        assert_eq!(
            ds.carried_dimensions_at(Grain::Underlying)
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            vec!["currency"]
        );
        assert!(ds.carried_dimensions_at(Grain::Position).is_empty());
    }

    #[test]
    fn categorical_defaults_true_for_dimensions_and_false_otherwise_and_attributes_may_opt_in() {
        let (schema, diags) = SchemaSpec::from_doc(&doc(CARRIED));
        assert!(diags.is_empty(), "{diags:?}");
        let ds = schema.dataset("risk").unwrap();
        assert!(ds.column("book").unwrap().categorical);
        assert!(ds.column("currency").unwrap().categorical);
        assert!(!ds.column("position_ref").unwrap().categorical, "keys are never categorical by default");
        assert!(!ds.column("npv").unwrap().categorical);
        assert!(ds.column("expiry").unwrap().categorical, "an attribute opted in");
        assert_eq!(
            ds.categorical_columns(),
            vec!["book", "lhu", "counterparty", "underlying_ref", "currency", "expiry"]
        );
    }

    #[test]
    fn a_dimension_may_opt_out_of_categorical() {
        let text = format!(
            "{CARRIED}\n[risk.columns.trade_ref]\ntype = \"utf8\"\nrole = \"dimension\"\ngrain = \"position\"\ncategorical = false\n"
        );
        let (schema, diags) = SchemaSpec::from_doc(&doc(&text));
        assert!(diags.is_empty(), "{diags:?}");
        let ds = schema.dataset("risk").unwrap();
        assert!(!ds.column("trade_ref").unwrap().categorical);
        assert!(ds.carries(Grain::Position, "trade_ref"), "still a dimension");
    }

    #[test]
    fn categorical_on_a_non_string_column_is_a_diagnostic_and_the_column_is_kept_uncategorical() {
        let text = format!(
            "{CARRIED}\n[risk.columns.strike]\ntype = \"f64\"\nrole = \"attribute\"\ngrain = \"instrument\"\ncategorical = true\n"
        );
        let (schema, diags) = SchemaSpec::from_doc(&doc(&text));
        assert!(
            diags.iter().any(|d| d.message.contains("strike") && d.message.contains("categorical")),
            "{diags:?}"
        );
        let ds = schema.dataset("risk").unwrap();
        assert!(!ds.column("strike").unwrap().categorical);
    }

    #[test]
    fn a_bare_dimension_outside_every_built_in_key_is_an_error_and_is_dropped() {
        let text = format!("{CARRIED}\n[risk.columns.desk]\ntype = \"utf8\"\nrole = \"dimension\"\n");
        let (schema, diags) = SchemaSpec::from_doc(&doc(&text));
        let d = diags
            .iter()
            .find(|d| d.message.contains("desk"))
            .unwrap_or_else(|| panic!("{diags:?}"));
        assert_eq!(d.severity, Severity::Error);
        assert!(d.message.contains("grain ="), "the fix is named: {}", d.message);
        assert!(schema.dataset("risk").unwrap().column("desk").is_none());
    }

    #[test]
    fn textual_on_a_column_no_grain_can_route_is_an_error_and_textual_is_cleared() {
        // underlying2_ref is in the pair grain's raw key but not its
        // dimension key (it is canonicalised), so nothing can route it.
        let text = format!(
            "{CARRIED}\n[risk.columns.underlying2_ref]\ntype = \"utf8\"\nrole = \"dimension\"\ntextual = true\n[risk.columns.cross_gamma02]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"underlying_pair\"\n"
        );
        let (schema, diags) = SchemaSpec::from_doc(&doc(&text));
        let d = diags
            .iter()
            .find(|d| d.message.contains("underlying2_ref") && d.message.contains("textual"))
            .unwrap_or_else(|| panic!("{diags:?}"));
        assert_eq!(d.severity, Severity::Error);
        let ds = schema.dataset("risk").unwrap();
        assert!(!ds.column("underlying2_ref").unwrap().textual);
        assert!(ds.column("underlying2_ref").is_some(), "the key column itself stays");
    }
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p geode-core schema::tests -- --nocapture 2>&1 | tail -30`
Expected: compile errors on `ColumnRole::Dimension { .. }`, `carried_grain`,
`carries`, `dimensions_at`, `carried_dimensions_at`, `categorical`.

- [ ] **Step 3: Implement the vocabulary in `column.rs`**

Replace the `ColumnRole` enum and extend `ColumnSpec`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnRole {
    /// Part of a grain key.
    Key,
    /// Scopeable and groupable. `None` is a column of a built-in grain
    /// key (`Grain::key_columns`); `Some(g)` is a *carried* dimension —
    /// one value per row of `g`'s key, carried by `g` and every finer
    /// grain, stored as a payload column, never added to a key
    /// (spec §3.3).
    Dimension { grain: Option<Grain> },
    /// A number, aggregated at its declared grain.
    Measure { grain: Grain, aggregate: Aggregate },
    /// A non-numeric property carried at a grain (strike, expiry).
    Attribute { grain: Grain },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnSpec {
    pub name: String,
    pub source_name: Option<String>,
    pub ty: ColumnType,
    pub required: bool,
    pub textual: bool,
    /// Small enough vocabulary to be an ENUM: interned at ingest, given a
    /// picker, and matched by dictionary in the text filter (spec §3.3).
    /// Defaults to `true` for a dimension, `false` otherwise.
    pub categorical: bool,
    pub role: ColumnRole,
}

impl ColumnSpec {
    pub fn source_name(&self) -> &str {
        self.source_name.as_deref().unwrap_or(&self.name)
    }

    /// The grain a measure or attribute is declared at. `None` for keys
    /// and for every dimension, carried or not: a carried dimension is
    /// reached through [`Self::carried_grain`] so the by-grain payload
    /// paths (`grains()`, `measures_at`, `attributes_at`) keep meaning
    /// "declared at exactly this grain".
    pub fn grain(&self) -> Option<Grain> {
        match self.role {
            ColumnRole::Measure { grain, .. } | ColumnRole::Attribute { grain } => Some(grain),
            ColumnRole::Key | ColumnRole::Dimension { .. } => None,
        }
    }

    /// `Some` for a carried dimension: the grain whose key determines it.
    pub fn carried_grain(&self) -> Option<Grain> {
        match self.role {
            ColumnRole::Dimension { grain } => grain,
            _ => None,
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
            categorical: true,
            role: ColumnRole::Dimension { grain: None },
        }
    }
}
```

Every existing `ColumnRole::Dimension` pattern in the workspace becomes
`ColumnRole::Dimension { .. }` or `ColumnRole::Dimension { grain: None }`
as the meaning requires. The full list (`grep -rn "ColumnRole::Dimension"
crates`): `schema/mod.rs:162`, `schema/column.rs:106,118`,
`store/ddl.rs:53,112`, plus every test fixture that builds a `ColumnSpec`
literal (add `categorical: true` for dimensions, `false` otherwise).

- [ ] **Step 4: Implement `DatasetSpec` helpers and the parse/validate rules in `schema/mod.rs`**

Add to `impl DatasetSpec`:

```rust
    /// Whether `grain` carries `column` as a dimension (spec §3.3): one
    /// of the grain's dimension keys, or a carried dimension whose
    /// declaring grain's key is contained in this grain's dimension key
    /// — which is how "every finer grain" is defined, and why the pair
    /// grain (dimension key = the instrument key) carries an
    /// instrument-grain dimension while the position grain does not.
    pub fn carries(&self, grain: Grain, column: &str) -> bool {
        if grain.dimension_key_columns().contains(&column) {
            return true;
        }
        self.column(column)
            .and_then(|c| c.carried_grain())
            .is_some_and(|declared| {
                declared
                    .key_columns()
                    .iter()
                    .all(|k| grain.dimension_key_columns().contains(k))
            })
    }

    /// The columns a view may group or scope by at `grain`: the dimension
    /// keys first, then every carried dimension, in schema order.
    pub fn dimensions_at(&self, grain: Grain) -> Vec<&str> {
        let mut out: Vec<&str> = grain.dimension_key_columns().to_vec();
        out.extend(self.carried_dimensions_at(grain).into_iter().map(|c| c.name.as_str()));
        out
    }

    /// Carried dimensions this grain's table stores as payload columns.
    pub fn carried_dimensions_at(&self, grain: Grain) -> Vec<&ColumnSpec> {
        self.columns
            .iter()
            .filter(|c| c.carried_grain().is_some() && self.carries(grain, &c.name))
            .collect()
    }

    /// Columns interned as ENUMs at ingest, pickable, and matched by
    /// dictionary in the text filter (spec §3.3).
    pub fn categorical_columns(&self) -> Vec<&str> {
        self.columns
            .iter()
            .filter(|c| c.categorical)
            .map(|c| c.name.as_str())
            .collect()
    }
```

In `parse_column`, replace the `"dimension"` arm and the struct literal:

```rust
        "dimension" => ColumnRole::Dimension {
            grain: match table.get("grain").and_then(|v| v.as_str()) {
                None => None,
                Some(g) => Some(Grain::parse(g).ok_or_else(|| bad(format!("unknown grain '{g}'")))?),
            },
        },
```

```rust
    let categorical_default = matches!(role, ColumnRole::Dimension { .. });
    let categorical = match table.get("categorical").and_then(|v| v.as_bool()) {
        None => categorical_default,
        Some(true) if ty != ColumnType::Utf8 => {
            return Err(bad(format!(
                "categorical = true needs type = \"utf8\" (got '{ty_str}'); \
                 only a string column can be an ENUM"
            )));
        }
        Some(v) => v,
    };
```

`parse_column` currently returns `Err` → the column is skipped. The test
expects `strike` to be *kept* with `categorical = false`, so instead of
`return Err(..)` for that one case, collect it: change `parse_column` to
return `Result<(ColumnSpec, Option<Diagnostic>), Diagnostic>` where the
`Option` is a non-fatal warning attached to a kept column, and have
`from_doc` push it. (The alternative, dropping `strike`, would silently
remove a real attribute over a flag typo — the wrong severity.)

Add two rules to `validate_dataset`, which must now return the columns to
drop as well as diagnostics. Change its signature to
`fn validate_dataset(ds: &mut DatasetSpec) -> Vec<Diagnostic>` and:

```rust
    // A bare dimension must be a column of some built-in grain key;
    // otherwise no table would carry it (ddl.rs) and every reference to
    // it would fail inside the query path. Error, and drop the column so
    // a view naming it gets the view validator's "unknown column".
    let bare_outside_key: Vec<String> = ds
        .columns
        .iter()
        .filter(|c| c.role == ColumnRole::Dimension { grain: None })
        .filter(|c| !Grain::ALL.iter().any(|g| g.key_columns().contains(&c.name.as_str())))
        .map(|c| c.name.clone())
        .collect();
    for name in &bare_outside_key {
        diags.push(Diagnostic {
            severity: Severity::Error,
            layer: None,
            file: None,
            message: format!(
                "dataset '{}' column '{name}': a dimension must be a built-in key column \
                 ({}) or declare the grain that carries it (`grain = \"instrument\"`); dropped",
                ds.name,
                Grain::UnderlyingPair.key_columns().join(", ")
            ),
        });
    }
    ds.columns.retain(|c| !bare_outside_key.contains(&c.name));

    // `textual` needs a grain that can evaluate the column: a dimension
    // some grain carries, or a measure/attribute declared at a grain.
    // Found by measurement (spec §7): one unroutable textual column
    // fails every text-filtered query on the dataset.
    let routable = |c: &ColumnSpec| {
        c.grain().is_some() || Grain::ALL.iter().any(|g| ds.carries(*g, &c.name))
    };
    let unroutable: Vec<String> = ds
        .columns
        .iter()
        .filter(|c| c.textual && !routable(c))
        .map(|c| c.name.clone())
        .collect();
    for name in &unroutable {
        diags.push(Diagnostic {
            severity: Severity::Error,
            layer: None,
            file: None,
            message: format!(
                "dataset '{}' column '{name}': textual = true, but no grain carries it as a \
                 dimension, so the text filter cannot route it; textual ignored",
                ds.name
            ),
        });
    }
    for c in &mut ds.columns {
        if unroutable.contains(&c.name) {
            c.textual = false;
        }
    }
```

The `routable` closure borrows `ds` immutably while `ds` is `&mut`;
compute `unroutable` before the `for c in &mut ds.columns` loop, which
the order above already does — take `let names: Vec<String>` first, then
mutate.

`from_doc` calls `diags.extend(validate_dataset(&mut dataset))` before
pushing the dataset.

- [ ] **Step 5: Run the schema tests**

Run: `cargo test -p geode-core schema:: 2>&1 | tail -20`
Expected: all pass, including the pre-existing ones.

- [ ] **Step 6: `attribution_of` takes the dataset**

In `crates/geode-core/src/attribution.rs`:

```rust
pub fn attribution_of(
    ds: &DatasetSpec,
    grain: Grain,
    grouping: &[String],
    dims: &DerivedDimensions,
) -> Attribution {
    // Dimension keys plus carried dimensions (spec §3.3): a carried
    // dimension is functionally determined by this grain's key, so
    // grouping by it partitions the rows exactly as a key does. The pair
    // grain's canonicalised underlyings are still excluded, for the
    // reason `dimension_key_columns` gives.
    let key = ds.dimensions_at(grain);
    // … unchanged from here, with `key.contains(c)` reading the Vec …
```

Update the test at `attribution.rs:202` to build a `DatasetSpec` (reuse
the `dataset()` helper pattern from `scope/mod.rs`'s tests: a TOML
fixture through `SchemaSpec::from_doc`) and add:

```rust
    #[test]
    fn grouping_by_a_carried_dimension_is_additive_where_carried_and_non_attributable_where_not() {
        let ds = carried_dataset(); // the CARRIED fixture from schema tests, duplicated here
        let g = |cols: &[&str]| cols.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            attribution_of(&ds, Grain::Underlying, &g(&["currency"]), &dims()),
            Attribution::Additive
        );
        assert_eq!(
            attribution_of(&ds, Grain::Instrument, &g(&["currency", "instrument_ref"]), &dims()),
            Attribution::Additive
        );
        assert_eq!(
            attribution_of(&ds, Grain::Position, &g(&["currency"]), &dims()),
            Attribution::NonAttributable,
            "a position spans currencies and nothing names the position"
        );
        assert_eq!(
            attribution_of(&ds, Grain::Position, &g(&["position_ref", "currency"]), &dims()),
            Attribution::DeterminedNonAdditive
        );
    }
```

Update the two callers in `compile.rs` (lines 532 and 786) to pass `ds`
(the view's dataset) and `joined_ds` respectively.

- [ ] **Step 7: Run core tests**

Run: `cargo test -p geode-core 2>&1 | tail -5`
Expected: green. `geode-data` does not compile yet.

- [ ] **Step 8: Write the failing data-layer tests**

In `crates/geode-data/src/ingest/split.rs` tests, the existing
`dataset()` fixture is a TOML string; add a carried dimension to it:

```toml
[risk_snapshot.columns.currency]
type = "utf8"
role = "dimension"
grain = "instrument"
```

and, if the fixture's raw table builder lists columns explicitly, a
`currency` column whose value is a function of `instrument_ref` (e.g.
`case when instrument_ref like '%1' then 'USD' else 'EUR' end`). Add:

```rust
    #[test]
    fn a_carried_dimension_lands_in_its_grain_and_every_finer_one_but_not_position() {
        let (_dir, store) = fixture();
        let ds = dataset();
        split_by_grain(store.writer(), &req(&ds)).unwrap();
        let has = |table: &str| -> bool {
            store
                .writer()
                .query_row(
                    &format!("select count(*) from (describe {table}) where column_name = 'currency'"),
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap()
                == 1
        };
        assert!(!has("staging_position"));
        assert!(has("staging_instrument"));
        assert!(has("staging_underlying"));
        // and the value is the one the instrument carries
        let n: i64 = store
            .writer()
            .query_row(
                "select count(*) from staging_instrument where currency is null",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn a_carried_dimension_that_varies_within_its_key_is_a_conflict() {
        let (_dir, store) = fixture();
        let ds = dataset();
        // Break the dependency for one instrument in the raw table.
        store
            .writer()
            .execute_batch(
                "update staging_raw set currency = 'JPY'
                 where rowid = (select min(rowid) from staging_raw)",
            )
            .unwrap();
        let out = split_by_grain(store.writer(), &req(&ds)).unwrap();
        assert!(
            out.conflicts
                .iter()
                .any(|c| c.column == "currency" && c.grain == Grain::Instrument && c.groups == 1),
            "{:?}",
            out.conflicts
        );
    }
```

(Adjust `staging_raw` and `rowid` to whatever the fixture's raw table is
named — read `fixture()` and `req()` first; the point is one row whose
currency disagrees with the rest of its instrument.)

In `crates/geode-data/src/store/ddl.rs` tests:

```rust
    #[test]
    fn create_table_carries_a_carried_dimension_at_its_grain_and_finer() {
        let ds = carried_dataset(); // TOML fixture with currency at instrument
        let sql = |g| create_table_sql(&ds, g, TableKind::Live);
        assert!(!sql(Grain::Position).contains("\"currency\""));
        assert!(sql(Grain::Instrument).contains("\"currency\" VARCHAR"));
        assert!(sql(Grain::Underlying).contains("\"currency\" VARCHAR"));
    }

    #[test]
    fn categorical_columns_follow_the_flag_not_the_role() {
        let ds = carried_dataset(); // includes `expiry` attribute with categorical = true
        assert_eq!(
            categorical_columns(&ds),
            vec!["book", "lhu", "counterparty", "underlying_ref", "currency", "expiry"]
        );
    }
```

In `crates/geode-data/src/query/scope_sql.rs` tests (the file has a
`dataset()`/`compile` helper pattern; follow it):

```rust
    #[test]
    fn a_selection_on_a_carried_dimension_is_direct_where_carried_and_probed_from_position() {
        let ds = carried_dataset();
        let scope = Scope {
            dimensions: vec![DimensionSelection {
                column: "currency".into(),
                values: vec!["USD".into()],
            }],
            ..Scope::default()
        };
        let at_instrument = compile_scope(&conn(), &scope, &ds, Grain::Instrument, &dims(), Era::live()).unwrap();
        assert!(at_instrument.predicate.contains("\"currency\" in"));
        assert!(!at_instrument.predicate.contains("exists"), "{}", at_instrument.predicate);
        assert_eq!(at_instrument.semantics, ScopeSemantics::Direct);

        let at_position = compile_scope(&conn(), &scope, &ds, Grain::Position, &dims(), Era::live()).unwrap();
        assert!(at_position.predicate.contains("exists"), "{}", at_position.predicate);
        assert_eq!(
            at_position.semantics,
            ScopeSemantics::SemiJoined { dimensions: vec!["currency".into()] },
            "positions that have USD risk, not the USD share of the position"
        );
    }
```

In `crates/geode-data/src/query/compile.rs` tests (there is an
end-to-end fixture that ingests a small CSV and runs `compile_view`; find
the test around line 1638, `grouping_by_a_derived_dimension_produces_its_mapped_values`,
and copy its setup):

```rust
    #[test]
    fn grouping_by_a_carried_dimension_sums_like_the_key_it_depends_on_and_blanks_coarser_measures() {
        let f = carried_fixture(); // ingests rows with currency per instrument; npv at position, delta01 at underlying
        let by_currency = run(&f, &["currency"]);       // helper returning (row_depth, currency, npv, delta01) rows
        let by_instrument = run(&f, &["instrument_ref"]);
        // delta01 (underlying grain) sums to the same total either way.
        assert_eq!(total(&by_currency, "delta01"), total(&by_instrument, "delta01"));
        // npv (position grain) is NonAttributable at the currency level: NULL.
        for row in by_currency.iter().filter(|r| r.depth == 1) {
            assert!(row.npv.is_none(), "position-grain npv must be blank under a currency grouping");
        }
        // and the grand total still carries it.
        assert!(by_currency.iter().any(|r| r.depth == 0 && r.npv.is_some()));
    }
```

Write `carried_fixture`, `run` and `total` in that test module against
the existing ingest+compile helpers there; the shape above is the
contract, the helper names are yours.

- [ ] **Step 9: Run them to see them fail**

Run: `cargo test -p geode-data 2>&1 | grep -E "^error|panicked|FAILED" | head -20`
Expected: compile errors (`ColumnRole::Dimension` patterns,
`attribution_of` arity) then, once those are fixed mechanically, the new
tests fail.

- [ ] **Step 10: Implement the data-layer sites**

`store/ddl.rs`:

```rust
/// Columns interned as ENUMs (spec §3.3, §3.6): the schema's
/// `categorical` flag, which defaults on for dimensions and off for
/// keys, whose vocabularies would make a useless dictionary.
pub fn categorical_columns(ds: &DatasetSpec) -> Vec<&str> {
    ds.categorical_columns()
}
```

Delete `dimension_columns` and update its two callers (`load.rs:244`,
`compile.rs:619`). In `create_table_sql`, after the measure/attribute
loop:

```rust
    // Carried dimensions (spec §3.3): payload columns of the declaring
    // grain's table and every finer grain's.
    for c in ds.carried_dimensions_at(grain) {
        cols.push(format!("  \"{}\" {}", c.name, c.ty.sql()));
    }
```

Update the module doc comment's paragraph about "A `Dimension` column
outside every grain key therefore appears in no table at all" to say
that such a column is now rejected at load, and that a dimension with a
`grain` is carried as payload.

`ingest/split.rs`, `payload_columns`:

```rust
/// Measures and attributes declared at this grain, plus the carried
/// dimensions it carries (spec §3.3). A carried dimension goes through
/// `any_value` like an attribute and — because the dependency on the
/// key is a claim the schema makes — through the conflict check below
/// like one too.
fn payload_columns(ds: &DatasetSpec, grain: Grain) -> Vec<&str> {
    let mut out: Vec<&str> = ds
        .columns
        .iter()
        .filter(|c| match c.role {
            ColumnRole::Measure { grain: g, .. } | ColumnRole::Attribute { grain: g } => g == grain,
            _ => false,
        })
        .map(|c| c.name.as_str())
        .collect();
    out.extend(ds.carried_dimensions_at(grain).into_iter().map(|c| c.name.as_str()));
    out
}
```

`ingest/load.rs`: the interning loop becomes

```rust
    for col in crate::store::ddl::categorical_columns(req.dataset) {
        // Refresh from the coarsest staged table carrying the column,
        // which is the smallest scan that sees every value.
        let Some(grain) = geode_core::schema::Grain::ALL.iter().find(|g| {
            (g.key_columns().contains(&col) || req.dataset.carries(**g, col)
                || req.dataset.column(col).and_then(|c| c.grain()) == Some(**g))
                && split.staged.iter().any(|(s, _)| s == *g)
        }) else {
            continue;
        };
        // … refresh_enum unchanged …
    }
```

(The third disjunct is what lets an opted-in *attribute* like `expiry`
refresh from its own grain's table.)

And in the `degradations` block, before `let health = …`:

```rust
    // A carried dimension that varied inside its key (spec §3.3): the
    // file disagrees with the schema's dependency claim. The row was
    // written with one of the values; say so rather than hide it.
    for c in &split.conflicts {
        if req.dataset.column(&c.column).and_then(|col| col.carried_grain()).is_some() {
            degradations.push(format!(
                "'{}' varies within its {:?} key in {} group(s)",
                c.column, c.grain, c.groups
            ));
        }
    }
```

Add a `load.rs` test that ingests a CSV where one instrument carries two
currencies and asserts `out.health` is `Degraded { reason }` with
`reason.contains("currency")`, following the `no book` test at line 843.

`query/scope_sql.rs`:

```rust
fn evaluable_at(ds: &DatasetSpec, dims: &DerivedDimensions, grain: Grain, column: &str) -> bool {
    let base = dims.base_column(column);
    ds.carries(grain, base) || ds.column(base).and_then(|c| c.grain()) == Some(grain)
}
```

and in `route`, the `carried` local becomes
`Grain::ALL.iter().any(|g| ds.carries(*g, base))`. `shared_keys` and
`is_membership` stay on keys: the probe semantics are about entity
identity, which a carried dimension never changes.

`query/compile.rs`:

```rust
fn carries_all(ds: &DatasetSpec, grain: Grain, columns: &[String], dims: &DerivedDimensions) -> bool {
    columns.iter().all(|col| ds.carries(grain, dims.base_column(col)))
}
```

`finest_carrying` passes `ds`; `derived_for` gains `ds: &DatasetSpec`
and filters on `ds.carries(grain, &d.from)`; the `own` filter at line
390 becomes `.filter(|g| ds.carries(grain, dims.base_column(g)))`; the
error text at line 569 becomes "a grouping column must be carried as a
dimension by some grain". Both `attribution_of` calls pass the dataset.

- [ ] **Step 11: Run the data tests**

Run: `cargo test -p geode-data 2>&1 | tail -5`
Expected: green.

- [ ] **Step 12: Update the demo schema**

In `examples/demo-config/datasets.toml`, change `expiry`, `currency` and
`model_code` to

```toml
[risk_snapshot.columns.expiry]
type = "utf8"
role = "dimension"
grain = "instrument"
textual = true
source_name = "Expiry"

[risk_snapshot.columns.currency]
type = "utf8"
role = "dimension"
grain = "instrument"
textual = true
source_name = "Currency"

[risk_snapshot.columns.model_code]
type = "utf8"
role = "dimension"
grain = "instrument"
textual = true
source_name = "ModelCode"
```

and add `textual = true` to `book`, `lhu`, `position_ref`,
`instrument_ref`, `underlying_ref`, `counterparty`. Not
`underlying2_ref` (unroutable, see the schema test) and not
`business_date`. Update the file's header comment: the columns changed,
so a persisted demo database must be deleted. Find where `--demo` puts
its database (`bridge::db_path` and its test
`the_database_path_prefers_config_then_demo_then_the_platform_dir`) and
record the path in the comment.

Add a test in `crates/geode-app/src/main.rs` (or wherever the demo
config layer is already tested — grep `demo-config` in `geode-app`'s
tests) asserting that `SchemaSpec::from_doc` over the compiled-in
`datasets` doc yields **no error diagnostics** and that
`risk_snapshot`'s `categorical_columns()` includes `currency`,
`model_code` and `expiry`.

Run: `cargo run -p geode-app -- --demo 20000` and confirm the blotter
paints; `:group currency,underlying_ref` in the tile groups by currency
with position-grain columns blank at the currency level.

- [ ] **Step 13: Harness entries**

Append to `scripts/mutation-check.sh` under a new
`# ---- carried dimensions (Phase 4 spec §3.3)` header:

```sh
run_mutation "carried: a carried dimension is a payload column of its grain and finer" \
  crates/geode-data/src/ingest/split.rs \
  '    out.extend(ds.carried_dimensions_at(grain).into_iter().map(|c| c.name.as_str()));' \
  '    let _ = ds.carried_dimensions_at(grain);' \
  geode-data a_carried_dimension_lands_in_its_grain_and_every_finer_one_but_not_position

run_mutation "carried: DDL carries a carried dimension" \
  crates/geode-data/src/store/ddl.rs \
  '    for c in ds.carried_dimensions_at(grain) {' \
  '    for c in ds.carried_dimensions_at(grain).into_iter().filter(|_| false) {' \
  geode-data create_table_carries_a_carried_dimension_at_its_grain_and_finer

run_mutation "carried: routing evaluates a carried dimension where it is carried" \
  crates/geode-data/src/query/scope_sql.rs \
  '    ds.carries(grain, base) || ds.column(base).and_then(|c| c.grain()) == Some(grain)' \
  '    grain.dimension_key_columns().contains(&base) || ds.column(base).and_then(|c| c.grain()) == Some(grain)' \
  geode-data a_selection_on_a_carried_dimension_is_direct_where_carried_and_probed_from_position

run_mutation "carried: attribution counts a carried dimension as part of the key" \
  crates/geode-core/src/attribution.rs \
  '    let key = ds.dimensions_at(grain);' \
  '    let key: Vec<&str> = grain.dimension_key_columns().to_vec();' \
  geode-core grouping_by_a_carried_dimension_is_additive_where_carried_and_non_attributable_where_not

run_mutation "carried: the grouping check accepts a carried dimension" \
  crates/geode-data/src/query/compile.rs \
  '    columns.iter().all(|col| ds.carries(grain, dims.base_column(col)))' \
  '    columns.iter().all(|col| grain.dimension_key_columns().contains(&dims.base_column(col)))' \
  geode-data grouping_by_a_carried_dimension_sums_like_the_key_it_depends_on_and_blanks_coarser_measures

run_mutation "carried: a dependency violation degrades health" \
  crates/geode-data/src/ingest/load.rs \
  '        if req.dataset.column(&c.column).and_then(|col| col.carried_grain()).is_some() {' \
  '        if false {' \
  geode-data <name of the load.rs test from Step 10>

run_mutation "categorical: defaults on for dimensions, off otherwise" \
  crates/geode-core/src/schema/mod.rs \
  '    let categorical_default = matches!(role, ColumnRole::Dimension { .. });' \
  '    let categorical_default = true;' \
  geode-core categorical_defaults_true_for_dimensions_and_false_otherwise_and_attributes_may_opt_in

run_mutation "categorical: interning follows the flag" \
  crates/geode-data/src/store/ddl.rs \
  '    ds.categorical_columns()' \
  '    ds.columns.iter().filter(|c| matches!(c.role, ColumnRole::Dimension { .. })).map(|c| c.name.as_str()).collect()' \
  geode-data categorical_columns_follow_the_flag_not_the_role

run_mutation "schema: a bare dimension outside every key is dropped" \
  crates/geode-core/src/schema/mod.rs \
  '    ds.columns.retain(|c| !bare_outside_key.contains(&c.name));' \
  '    let _ = &bare_outside_key;' \
  geode-core a_bare_dimension_outside_every_built_in_key_is_an_error_and_is_dropped

run_mutation "schema: textual on an unroutable column is cleared" \
  crates/geode-core/src/schema/mod.rs \
  '        if unroutable.contains(&c.name) {' \
  '        if false {' \
  geode-core textual_on_a_column_no_grain_can_route_is_an_error_and_textual_is_cleared
```

Run: `zsh scripts/mutation-check.sh --changed`
Expected: every new entry `CAUGHT`; every pre-existing entry whose file
changed still `CAUGHT`; no `ANCHOR-MISSING`. An anchor that moved (the
`interned` list in `compile.rs` used `dimension_columns`) is updated in
the same commit.

- [ ] **Step 14: Full checks and commit**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace 2>&1 | tail -3 && cargo bench --workspace --no-run 2>&1 | tail -1 && cargo check -p geode-shell --features test-support --all-targets`

```bash
git add -A
git commit -m "feat(schema): carried dimensions and the categorical flag (Phase 4 §3.3)

A dimension may name the grain whose key determines it and is then
carried by that grain and every finer one as a payload column: grouped,
scoped, picked and ENUM-interned like a key dimension, never added to a
key. Interning, pickers and the text rewrite read one categorical flag.
The demo's currency, model_code and expiry become carried dimensions."
```

---

### Task 2: `Request::Distinct`, the ENUM text rewrite, and the bench

Spec §3.4, §3.5, §7. The distinct-values query the pickers need, the
dictionary rewrite that makes a per-keystroke text filter affordable,
and the permanent bench cases that gate it.

**Files:**
- Modify: `crates/geode-core/src/query.rs`
- Create: `crates/geode-data/src/query/distinct.rs`
- Modify: `crates/geode-data/src/query/mod.rs` (add `pub mod distinct;`)
- Modify: `crates/geode-data/src/query/pool.rs:37-60` (`QueryRequest`, `QueryResult`)
- Modify: `crates/geode-data/src/query/compile.rs` (extract `era_for`; move `existing_enum_types` to `ddl.rs`)
- Modify: `crates/geode-data/src/query/scope_sql.rs:335-365` (text block), `compile_scope` signature use of `conn`
- Modify: `crates/geode-data/src/store/ddl.rs` (receives `existing_enum_types`)
- Modify: `crates/geode-data/src/service.rs` (`DataEvent::Distinct`, `distinct`, the sink)
- Modify: `crates/geode-data/src/handle.rs` (`Request::Distinct`, `DataHandle::distinct`)
- Modify: `crates/geode-data/benches/query.rs`
- Modify: `scripts/mutation-check.sh`, `docs/perf.md`

**Interfaces:**
- Consumes: `compile_scope`, `Era`, `run_one`, `QueryPool::submit`,
  `ds.categorical_columns()`, `ds.carries`.
- Produces:
  ```rust
  // geode_core::query
  #[derive(Debug, Clone)]
  pub struct DistinctParams {
      pub key: QueryKey,
      pub tag: u64,
      pub column: String,
      /// The frame's scope with this column's own selection removed —
      /// the caller does the removal (spec §3.4).
      pub scope: Scope,
      pub as_of: AsOf,
  }
  #[derive(Debug)]
  pub struct DistinctOutcome {
      pub key: QueryKey,
      pub tag: u64,
      pub column: String,
      /// Sorted by value. `Err` is the failure text.
      pub values: Result<Vec<(String, u64)>, String>,
  }
  // geode_data
  pub enum Request { Query(QueryParams), Distinct(DistinctParams), Cancel { key }, ReplaceViews { .. }, Shutdown }
  pub enum DataEvent { Query(QueryOutcome), Distinct(DistinctOutcome), Published { .. }, Health { .. }, Diagnostics(..) }
  impl DataHandle { pub fn distinct(&self, params: DistinctParams) -> bool; }
  impl DataService { pub fn distinct(&self, params: &DistinctParams) -> Result<QueryId, StoreError>; }
  // geode_data::query::pool
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub enum RequestKind { Query, Distinct { column: String } }
  pub struct QueryRequest { /* … */ pub kind: RequestKind }
  pub struct QueryResult  { /* … */ pub kind: RequestKind }
  // geode_data::query::distinct
  pub fn compile_distinct(conn: &Connection, schema: &SchemaSpec, dims: &DerivedDimensions, params: &DistinctParams) -> Result<CompiledQuery, StoreError>;
  // geode_data::query::compile (extracted, pub(crate))
  pub(crate) struct ResolvedEra { pub kind: TableKind, pub generations: Option<String>, pub resolved_as_of: BTreeMap<String, DateTime<Utc>> }
  pub(crate) fn era_for(conn: &Connection, dataset: &str, ds: &DatasetSpec, as_of: &AsOf) -> Result<ResolvedEra, StoreError>;
  // geode_data::store::ddl
  pub fn existing_enum_types(conn: &Connection, dataset: &str) -> Result<Vec<String>, StoreError>;
  ```

- [ ] **Step 1: Shared types in `geode-core`**

Add `DistinctParams` and `DistinctOutcome` (above) to
`crates/geode-core/src/query.rs`, deriving as shown. Then, in the same
file, move `parse_as_of` here from
`crates/geode-blotter/src/core/commands.rs:169-190` verbatim (it depends
only on `chrono`, which `geode-core` already has), as
`pub fn parse_as_of(text: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>, String>`,
and make the blotter's `commands.rs` `pub use geode_core::query::parse_as_of;`
so its tests and callers keep compiling. Extend it to accept `HH:MM:SS`
(`NaiveTime::parse_from_str(text, "%H:%M:%S")`) before the RFC 3339
fallback, and add a test for that form beside the existing ones (move
them with the function).

Run: `cargo test -p geode-core query:: && cargo test -p geode-blotter commands 2>&1 | tail -3`
Expected: green.

- [ ] **Step 2: Extract `era_for` in `compile.rs`**

`compile_view` builds the generation predicate for an as-of query from
`history_of(&view.dataset, ds)` and the requested instant (the harness
anchor `&history_of(&view.dataset, ds), *t)` marks the site), and
records `resolved_as_of`. Lift that block into

```rust
pub(crate) struct ResolvedEra {
    pub kind: TableKind,
    pub generations: Option<String>,
    pub resolved_as_of: BTreeMap<String, DateTime<Utc>>,
}

impl ResolvedEra {
    pub(crate) fn era(&self) -> Era<'_> {
        Era { kind: self.kind, generations: self.generations.as_deref() }
    }
}

pub(crate) fn era_for(
    conn: &Connection,
    dataset: &str,
    ds: &DatasetSpec,
    as_of: &AsOf,
) -> Result<ResolvedEra, StoreError> { /* the lifted block */ }
```

and have `compile_view` call it. The harness anchor above must still
match after the move (the expression is inside `era_for` now) — run
`zsh scripts/mutation-check.sh "as-of"` after this step and fix the
anchor if it moved.

Move `existing_enum_types` from `compile.rs:252` to `store/ddl.rs` as
`pub fn`, updating `compile.rs`'s one call.

Run: `cargo test -p geode-data 2>&1 | tail -3 && zsh scripts/mutation-check.sh "as-of"`
Expected: green; `as-of` entries `CAUGHT`.

- [ ] **Step 3: Failing test for the ENUM rewrite**

In `scope_sql.rs` tests, add a fixture that actually has an ENUM type:
open a `Store`, `apply_schema` the carried dataset, insert a few rows
into the instrument live table with `book` values, run
`refresh_enum(conn, "risk", "book", "risk_instrument_live")`, then:

```rust
    #[test]
    fn a_text_filter_over_a_categorical_column_matches_the_dictionary_not_the_rows() {
        let (store, ds) = enum_fixture(); // books BK000..BK019 live; `book` categorical + textual
        let scope = Scope { text: Some("bk00".into()), ..Scope::default() };
        let sql = compile_scope(store.writer(), &scope, &ds, Grain::Instrument, &dims(), Era::live()).unwrap();
        assert!(
            sql.predicate.contains("enum_range(null::risk_book_enum)"),
            "{}", sql.predicate
        );
        assert!(sql.predicate.contains("ilike ?"), "the pattern is still bound: {}", sql.predicate);
        // And it selects the same rows as the row scan would.
        let via_dict: i64 = count(store.writer(), &ds, &sql);
        let row_scan: i64 = store
            .writer()
            .query_row(
                "select count(*) from risk_instrument_live where \"book\" ilike '%bk00%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(via_dict, row_scan);
        assert!(via_dict > 0);
    }

    #[test]
    fn the_rewrite_falls_back_to_the_row_scan_under_as_of_and_when_the_type_is_missing() {
        let (store, ds) = enum_fixture();
        let scope = Scope { text: Some("bk00".into()), ..Scope::default() };
        let archive = Era { kind: TableKind::Archive, generations: Some("true") };
        let sql = compile_scope(store.writer(), &scope, &ds, Grain::Instrument, &dims(), archive).unwrap();
        assert!(!sql.predicate.contains("enum_range"), "{}", sql.predicate);
        // Drop the type: the live path must not name a type that is not there.
        store.writer().execute_batch("drop type risk_book_enum").unwrap();
        let sql = compile_scope(store.writer(), &scope, &ds, Grain::Instrument, &dims(), Era::live()).unwrap();
        assert!(!sql.predicate.contains("enum_range"), "{}", sql.predicate);
    }

    proptest! {
        #[test]
        fn dictionary_and_row_scan_agree_for_any_needle(needle in "[a-zA-Z0-9%_\\\\]{0,6}") {
            let (store, ds) = enum_fixture();
            let scope = Scope { text: Some(needle.clone()), ..Scope::default() };
            let sql = compile_scope(store.writer(), &scope, &ds, Grain::Instrument, &dims(), Era::live()).unwrap();
            let via_dict = count(store.writer(), &ds, &sql);
            let pattern = like_pattern(&needle);
            let row_scan: i64 = store.writer().query_row(
                "select count(*) from risk_instrument_live where \"book\" ilike ? escape '\\'",
                duckdb::params![pattern], |r| r.get(0)).unwrap();
            prop_assert_eq!(via_dict, row_scan);
        }
    }
```

`proptest` is a dev-dependency of `geode-data` already if the scope
lowering has property tests (spec §10.3 says it does — check
`crates/geode-data/Cargo.toml` `[dev-dependencies]`; add
`proptest = "1"` if absent). `count` runs
`select count(*) from risk_instrument_live where {predicate}` binding
`sql.params`.

Run: `cargo test -p geode-data scope_sql::tests::a_text_filter_over 2>&1 | tail -5`
Expected: FAIL — the predicate has no `enum_range`.

- [ ] **Step 4: Implement the rewrite**

In `compile_scope`, rename `_conn` to `conn` and, at the top of the text
block:

```rust
    if let Some(text) = &scope.text {
        let pattern = Value::Text(like_pattern(text));
        // Dictionary terms (spec §3.5): a categorical column is ENUM-typed
        // in the live era, so the pattern is evaluated over the type's
        // values and the row test becomes an `in`, which DuckDB runs on
        // the codes. Only when the type exists: before the first load it
        // does not, and naming it would fail the statement.
        let enum_types = if era.kind == TableKind::Live {
            crate::store::ddl::existing_enum_types(conn, &ds.name)?
        } else {
            Vec::new()
        };
        let mut terms: Vec<String> = Vec::new();
        let mut term_params: Vec<Value> = Vec::new();
        for col in ds.textual_columns() {
            let name = col.name.as_str();
            let ty = crate::store::ddl::enum_type_name(&ds.name, name);
            let test = if col.categorical && enum_types.contains(&ty) {
                format!(
                    "\"{name}\" in (select v from unnest(enum_range(null::{ty})) t(v) \
                     where v ilike ? escape '\\')"
                )
            } else {
                format!("\"{name}\" ilike ? escape '\\'")
            };
            // … routing unchanged …
```

`ds.name` is what `enum_type_name` keys on; confirm against
`load.rs`'s `refresh_enum(conn, req.dataset_name, col, ..)` that the
dataset name used at ingest is the schema's `ds.name` (it is
`req.dataset_name`, which `LoadRequest` gets from the same spec — verify
by execution: the first test above only passes if the names line up).

Run: `cargo test -p geode-data scope_sql 2>&1 | tail -5`
Expected: green, including the property test.

- [ ] **Step 5: Failing tests for `Distinct`**

In the new `crates/geode-data/src/query/distinct.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinct_counts_values_under_the_given_scope_and_unions_datasets() {
        let f = two_dataset_fixture(); // risk (book, currency at instrument) + ref (currency at instrument)
        let params = DistinctParams {
            key: QueryKey(1),
            tag: 1,
            column: "currency".into(),
            scope: book_scope("BK000"),
            as_of: AsOf::Live,
        };
        let compiled = compile_distinct(f.conn(), &f.schema, &f.dims, &params).unwrap();
        let rows = f.run(&compiled); // Vec<(String, u64)> via run_one + text_at/i64_at
        assert_eq!(rows, vec![("EUR".to_string(), 3), ("USD".to_string(), 5)]);
        // A book selection narrowed the counts; unscoped they are larger.
        let unscoped = DistinctParams { scope: Scope::default(), ..params.clone() };
        let all = f.run(&compile_distinct(f.conn(), &f.schema, &f.dims, &unscoped).unwrap());
        assert!(all.iter().map(|(_, n)| n).sum::<u64>() > 8);
    }

    #[test]
    fn distinct_over_an_unknown_column_is_an_error_not_a_binder_failure() {
        let f = two_dataset_fixture();
        let params = DistinctParams { column: "nope".into(), ..base_params() };
        let e = compile_distinct(f.conn(), &f.schema, &f.dims, &params).unwrap_err();
        assert!(e.to_string().contains("nope"));
    }

    #[test]
    fn distinct_under_as_of_reads_the_archive_era() {
        let f = two_dataset_fixture_with_history(); // two generations, currency changed between them
        let params = DistinctParams { as_of: AsOf::At(f.between), ..base_params() };
        let rows = f.run(&compile_distinct(f.conn(), &f.schema, &f.dims, &params).unwrap());
        assert!(rows.iter().any(|(v, _)| v == "GBP"), "the older generation's value: {rows:?}");
    }
}
```

Build the fixtures on the same ingest helpers `compile.rs`'s tests use.

Run: `cargo test -p geode-data distinct 2>&1 | tail -3`
Expected: compile error, `compile_distinct` missing.

- [ ] **Step 6: Implement `compile_distinct`**

```rust
//! The picker's distinct-values query (Phase 4 spec §3.4): per value,
//! how many rows the current scope would leave, over every dataset
//! carrying the column, under the same era routing every query uses.

use crate::query::compile::{CompiledQuery, era_for};
use crate::query::scope_sql::compile_scope;
use crate::store::StoreError;
use duckdb::Connection;
use duckdb::types::Value;
use geode_core::attribution::{Attribution, ScopeSemantics};
use geode_core::dimensions::DerivedDimensions;
use geode_core::query::DistinctParams;
use geode_core::schema::{Grain, SchemaSpec};
use geode_core::snapshot::ColumnMeta;

pub fn compile_distinct(
    conn: &Connection,
    schema: &SchemaSpec,
    dims: &DerivedDimensions,
    params: &DistinctParams,
) -> Result<CompiledQuery, StoreError> {
    let base = dims.base_column(&params.column);
    let mut selects: Vec<String> = Vec::new();
    let mut all_params: Vec<Value> = Vec::new();
    for ds in &schema.datasets {
        // The coarsest grain carrying the column: the smallest table
        // that sees every value.
        let Some(grain) = ds
            .grains()
            .into_iter()
            .find(|g| ds.carries(*g, base) || ds.column(base).and_then(|c| c.grain()) == Some(*g))
        else {
            continue;
        };
        let era = era_for(conn, &ds.name, ds, &params.as_of)?;
        let scope = compile_scope(conn, &params.scope, ds, grain, dims, era.era())?;
        let derived = dims.get(&params.column);
        let value_expr = match derived {
            None => format!("\"{base}\"::varchar"),
            Some(d) => crate::query::compile::derived_case(d), // the `case` the tree compiler emits for a derived dimension — expose it pub(crate)
        };
        selects.push(format!(
            "select {value_expr} as value, count(*) as n from {} where {} group by 1",
            era.era().relation(&ds.name, grain),
            match era.era().generations {
                Some(g) => format!("({}) and ({g})", scope.predicate),
                None => scope.predicate.clone(),
            }
        ));
        all_params.extend(scope.params);
    }
    if selects.is_empty() {
        return Err(StoreError::Sql {
            statement: format!("distinct '{}'", params.column),
            source: duckdb::Error::InvalidParameterName(format!(
                "no dataset carries '{}'",
                params.column
            )),
        });
    }
    let sql = format!(
        "select value, sum(n)::bigint as n from ({}) u where value is not null group by 1 order by 1",
        selects.join(" union all ")
    );
    Ok(CompiledQuery {
        sql,
        params: all_params,
        grouping: Vec::new(),
        columns: vec![meta("value"), meta("n")],
        stalest_input: Vec::new(),
        resolved_as_of: Default::default(),
    })
}

fn meta(name: &str) -> ColumnMeta {
    ColumnMeta {
        name: name.into(),
        attribution_by_depth: vec![Attribution::Additive],
        scope_semantics: ScopeSemantics::Direct,
    }
}
```

`derived_case`: the tree compiler already renders a derived dimension as
a `case` expression (`derived_expr` at `compile.rs:199`); make that
`pub(crate)` and call it. Check `Era::relation` for the archive form
already includes the union with live.

- [ ] **Step 7: Route it through the pool and the service**

`pool.rs`: add `RequestKind` (above), a `kind` field on `QueryRequest`
and `QueryResult`, copied through in the worker's delivery site. Every
existing constructor sets `kind: RequestKind::Query`.

`service.rs`:

```rust
    pub fn distinct(&self, params: &DistinctParams) -> Result<QueryId, StoreError> {
        let compiled = compile_distinct(
            &self.conn,
            &self.config.schema,
            &self.config.dimensions,
            params,
        )?;
        Ok(self.pool.submit(QueryRequest {
            key: params.key,
            tag: params.tag,
            submitted: Instant::now(),
            view: ViewId(format!("distinct:{}", params.column)),
            grouping: Vec::new(),
            compiled,
            provenance: Provenance::default(),
            kind: RequestKind::Distinct { column: params.column.clone() },
        }))
    }
```

and the result sink:

```rust
            Arc::new(move |r: QueryResult| match r.kind {
                RequestKind::Query => sink(DataEvent::Query(QueryOutcome { /* as today */ })),
                RequestKind::Distinct { column } => sink(DataEvent::Distinct(DistinctOutcome {
                    key: r.key,
                    tag: r.tag,
                    column,
                    values: r.snapshot.map(|s| {
                        let v = s.column_index("value").expect("distinct selects value");
                        let n = s.column_index("n").expect("distinct selects n");
                        (0..s.rows())
                            .filter_map(|row| {
                                Some((s.text_at(v, row)?.to_string(), s.i64_at(n, row)? as u64))
                            })
                            .collect()
                    }),
                })),
            })
```

`handle.rs`: `Request::Distinct(DistinctParams)`,
`DataHandle::distinct`, and the `serve` arm mirroring `Query`'s
compile-failure delivery (`DataEvent::Distinct` with `values: Err(..)`).

Add a `handle.rs` test in the style of
`a_test_handle_hands_requests_to_the_test` for `distinct`, and a
`service.rs` test that spawns a real service over the distinct fixture,
sends `Request::Distinct`, and receives `DataEvent::Distinct` with the
expected values.

Run: `cargo test -p geode-data 2>&1 | tail -3`
Expected: green.

- [ ] **Step 8: The bench, permanently**

In `crates/geode-data/benches/query.rs`:

1. Declare `textual = true` on `book`, `lhu`, `counterparty` and
   `underlying_ref` in `schema()` (the four categorical, routable
   columns; `underlying2_ref` stays untextual — Task 1's rule would
   reject it).
2. Add a second schema text, `schema_with_textual_keys()`, identical
   plus `textual = true` on `business_date`, `position_ref` and
   `instrument_ref` (the plain-string columns).
3. Add `fn reopen(db: &TempDir, src: &TempDir, schema: SchemaSpec) -> (DataService, Receiver<DataEvent>)`
   that opens a new service over the same database path with the given
   schema (no ingest; `apply_schema` is idempotent). `service()` returns
   what it needs for that.
4. After the existing functions in the `for rows in …` loop:

```rust
        for (label, needle) in [("broad", "bk00"), ("narrow", "bk007"), ("none", "zzz")] {
            let text_only = Scope { text: Some(needle.to_string()), ..Scope::default() };
            let text_and_books = Scope { text: Some(needle.to_string()), ..book_scope() };
            group.bench_function(format!("{rows}_rows_text_{label}_unscoped"), |b| {
                b.iter(|| black_box(requery(&svc, &rx, "tree", &text_only, usize::MAX)))
            });
            group.bench_function(format!("{rows}_rows_text_{label}_depth_2"), |b| {
                b.iter(|| black_box(requery(&svc, &rx, "tree", &text_only, 2)))
            });
            group.bench_function(format!("{rows}_rows_text_{label}_with_books"), |b| {
                b.iter(|| black_box(requery(&svc, &rx, "tree", &text_and_books, usize::MAX)))
            });
        }
        svc.shutdown();

        // The same needles with the plain-string key columns textual too:
        // the residual row scan the dictionary rewrite cannot remove.
        let (svc, rx) = reopen(&_db, &_src, schema_with_textual_keys());
        for (label, needle) in [("none", "zzz")] {
            let text_only = Scope { text: Some(needle.to_string()), ..Scope::default() };
            group.bench_function(format!("{rows}_rows_text_{label}_keys_textual_depth_2"), |b| {
                b.iter(|| black_box(requery(&svc, &rx, "tree", &text_only, 2)))
            });
        }
        svc.shutdown();
```

Run: `cargo bench -p geode-data --bench query -- "1000000_rows_text" 2>&1 | grep -A1 "^query_requery"`

**The gate:** `1000000_rows_text_none_depth_2` (ENUM columns only) is
under 50 ms. Before the rewrite this shape measured 63 ms with eight
textual columns (spec §7). If it is not under 50 ms with the subquery
form, implement the fallback the spec names: resolve the dictionary in
Rust once per compile (`select unnest(enum_range(null::{ty}))`, filter
with the same escaping semantics via a `ILIKE`-equivalent matcher, bind
the literal list as one delimiter-joined varchar exactly as dimension
selections do) and re-measure. Record both numbers.

- [ ] **Step 9: Record and enter the harness**

`docs/perf.md`: add a `## Phase 4a: the text filter` section with the
table from spec §7 (before) and the post-rewrite numbers for every
`text_*` case, the `keys_textual` residual, and which form (subquery or
literal list) shipped.

Harness entries:

```sh
run_mutation "text: a categorical column matches the dictionary, not the rows" \
  crates/geode-data/src/query/scope_sql.rs \
  '            let test = if col.categorical && enum_types.contains(&ty) {' \
  '            let test = if false {' \
  geode-data a_text_filter_over_a_categorical_column_matches_the_dictionary_not_the_rows

run_mutation "text: the rewrite is live-era only" \
  crates/geode-data/src/query/scope_sql.rs \
  '        let enum_types = if era.kind == TableKind::Live {' \
  '        let enum_types = if true {' \
  geode-data the_rewrite_falls_back_to_the_row_scan_under_as_of_and_when_the_type_is_missing

run_mutation "text: the dictionary term keeps the escape clause" \
  crates/geode-data/src/query/scope_sql.rs \
  "                     where v ilike ? escape '\\\\')" \
  "                     where v ilike ?)" \
  geode-data dictionary_and_row_scan_agree_for_any_needle

run_mutation "distinct: counts are taken under the given scope" \
  crates/geode-data/src/query/distinct.rs \
  '        let scope = compile_scope(conn, &params.scope, ds, grain, dims, era.era())?;' \
  '        let scope = compile_scope(conn, &Scope::default(), ds, grain, dims, era.era())?;' \
  geode-data distinct_counts_values_under_the_given_scope_and_unions_datasets

run_mutation "distinct: as-of reads the archive era" \
  crates/geode-data/src/query/distinct.rs \
  '        let era = era_for(conn, &ds.name, ds, &params.as_of)?;' \
  '        let era = era_for(conn, &ds.name, ds, &AsOf::Live)?;' \
  geode-data distinct_under_as_of_reads_the_archive_era

run_mutation "distinct: the sink maps a Distinct result to a Distinct event" \
  crates/geode-data/src/service.rs \
  '                RequestKind::Distinct { column } => sink(DataEvent::Distinct(DistinctOutcome {' \
  '                RequestKind::Distinct { column: _ } => sink(DataEvent::Diagnostics(Vec::new())) || sink(DataEvent::Distinct(DistinctOutcome { column: String::new(),' \
  geode-data <the service.rs distinct round-trip test>
```

(The last one's replacement must still compile; if it does not, mutate
the `values:` mapping to `Ok(Vec::new())` instead — the point is that a
delivered `Distinct` with the wrong payload is caught.)

Run: `zsh scripts/mutation-check.sh --changed`, then the four CI checks.

```bash
git add -A
git commit -m "feat(data): Request::Distinct and the ENUM dictionary text rewrite (Phase 4 §3.4-3.5)

The picker's distinct-values query runs on the pool under the frame's
scope and era, unioned across datasets. A textual categorical column's
ILIKE now runs over its ENUM's values and becomes an IN over codes; the
bench gates the zero-match case at 1M rows, with the plain-string
residual recorded in docs/perf.md."
```

---

### Task 3: `Frame` grows up — undo/redo, previous as-of, recent publishes, saved scopes, the bar model, session `[frame]`

Spec §3.1 (model), §3.6 (presets, undo), §3.8, §3.9, §3.12.

**Files:**
- Modify: `crates/geode-core/src/scope/expr.rs` (Display)
- Create: `crates/geode-core/src/scopes.rs`; modify `lib.rs` (`pub mod scopes;`)
- Create: `crates/geode-shell/src/scopebar.rs`; modify `lib.rs`
- Modify: `crates/geode-shell/src/frame.rs`
- Modify: `crates/geode-shell/src/session.rs`, `shell/session_io.rs`, `shell/mod.rs` (`ShellServices::restored_frame`, `on_frame_changed` drains), `shell/hot_reload.rs` (`scopes` reload), `shell/toolbar.rs` + `shell/render.rs` (consume `bar_model`), `shell/tests/*` (readout assertions)
- Modify: `crates/geode-app/src/bridge.rs` (`Published` details), `main.rs` (restored frame)
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Produces:
  ```rust
  // geode_core::scope::expr
  impl std::fmt::Display for Expr { /* grammar text; parse_expr(render) == self */ }
  // geode_core::scopes
  pub type SavedScopes = BTreeMap<String, Scope>;
  pub fn saved_scopes_from_doc(doc: &MergedDoc, schema: &SchemaSpec, dims: &DerivedDimensions) -> (SavedScopes, Vec<Diagnostic>);
  pub fn scope_to_table(scope: &Scope) -> toml_edit::Table;
  // geode_shell::scopebar
  pub struct Chip { pub column: String, pub summary: String }
  pub struct ScopeBarModel {
      pub slot: Option<(u8, String)>,
      pub chips: Vec<Chip>,
      pub text: Option<String>,
      pub expr: Option<String>,        // elided source, ≤ 40 chars + "…"
      pub impossible: Option<String>,  // "∅ book"
      pub as_of: Option<String>,       // "14:05" local, or "2026-09-05 14:05" if not today
  }
  pub fn build_model(frame: &Frame, now: DateTime<Local>) -> ScopeBarModel;
  // geode_shell::frame
  pub const UNDO_DEPTH: usize = 32;
  pub const RECENT_PUBLISHES: usize = 32;
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct Publish { pub dataset: String, pub batch: String, pub books: usize, pub at: DateTime<Utc> }
  impl Frame {
      pub fn new(slots: GroupingSlots, saved: SavedScopes, user_dir: Option<PathBuf>) -> Frame;
      pub fn set_scope(&mut self, scope: Scope) -> bool;          // pushes undo, clears redo
      pub fn begin_scope_session(&mut self);                        // text field focus: next set_scope_in_session pushes once
      pub fn set_scope_in_session(&mut self, scope: Scope) -> bool; // pushes only the first time per session
      pub fn end_scope_session(&mut self);
      pub fn undo_scope(&mut self) -> bool;
      pub fn redo_scope(&mut self) -> bool;
      pub fn drop_dimension(&mut self, column: &str) -> bool;
      pub fn set_text(&mut self, text: Option<String>) -> bool;
      pub fn set_as_of(&mut self, as_of: AsOf) -> bool;           // remembers previous
      pub fn undo_as_of(&mut self) -> bool;
      pub fn note_published(&mut self, publish: Publish);
      pub fn recent_publishes(&self) -> &VecDeque<Publish>;
      pub fn saved_scopes(&self) -> &SavedScopes;
      pub fn replace_saved_scopes(&mut self, saved: SavedScopes) -> bool;   // bumps config
      pub fn save_scope(&mut self, name: &str) -> Result<(), String>;      // in memory + pending persist
      pub fn load_scope(&mut self, name: &str) -> Result<bool, String>;
      pub fn take_pending_scope_persist(&mut self) -> Option<(String, Scope)>;
      pub fn bar_model(&self) -> Rc<ScopeBarModel>;                       // cached on versions()
  }
  pub fn persist_scope_to_user_config(user_dir: &Path, name: &str, scope: &Scope) -> Result<(), String>;
  // geode_shell::session
  pub struct FrameRecord { pub scope: Scope, pub active_slot: Option<u8>, pub as_of: AsOf }
  pub fn to_toml(workspaces: &Workspaces, tiles: &TileRecords, frame: Option<&FrameRecord>) -> toml::Table;
  pub struct Restored { /* … */ pub frame: Option<FrameRecord> }
  // ShellServices gains `pub restored_frame: Option<FrameRecord>`
  ```

- [ ] **Step 1: `Expr` renders back to grammar text**

Read `parse_expr`'s string-literal lexing in `expr.rs` (how a quote
inside a string is escaped, whether double quotes are accepted) and
write the inverse. Test first, in `expr.rs`'s `mod tests`:

```rust
    #[test]
    fn rendering_an_expression_round_trips_through_the_parser() {
        for src in [
            "model_code = 'EURP'",
            "model_code = 'EURP' and underlying_ref = 'SPX'",
            "not (book = 'BK001' or lhu = 'X')",
            "npv > 100.5",
            "strike <= -3",
            "book in ('A', 'B')",
            "name ilike 'sp%'",   // adjust to the grammar's spelling of Like
            "flag = true",
            "note = 'it''s'",     // adjust to the grammar's escape
        ] {
            let e = parse_expr(src).unwrap_or_else(|err| panic!("{src}: {err}"));
            let rendered = e.to_string();
            let again = parse_expr(&rendered).unwrap_or_else(|err| panic!("{rendered}: {err}"));
            assert_eq!(e, again, "{src} -> {rendered}");
        }
    }
```

Implement `impl Display for Expr` with full parenthesisation of `And`/
`Or`/`Not` operands (never rely on precedence when rendering — the
round trip is what matters, not brevity), `Literal::Str` quoted with the
parser's escape, `Num` via `{}` on `f64` (a whole number renders without
a trailing `.0` only if the parser accepts both; otherwise keep `.0`),
`Bool` as `true`/`false`.

Run: `cargo test -p geode-core expr 2>&1 | tail -3`

- [ ] **Step 2: `ScopeSpec` reader and writer**

`crates/geode-core/src/scopes.rs`:

```rust
//! Saved scopes (Phase 4 spec §3.9): `scopes.toml`, a doc atomic at depth
//! one, each named scope replaced whole by a finer layer.

use crate::config::{Diagnostic, MergedDoc, Severity};
use crate::dimensions::DerivedDimensions;
use crate::schema::SchemaSpec;
use crate::scope::{DimensionSelection, Scope, parse_expr};
use std::collections::BTreeMap;

pub type SavedScopes = BTreeMap<String, Scope>;

pub fn saved_scopes_from_doc(
    doc: &MergedDoc,
    schema: &SchemaSpec,
    dims: &DerivedDimensions,
) -> (SavedScopes, Vec<Diagnostic>) {
    let mut out = SavedScopes::new();
    let mut diags = Vec::new();
    let warn = |m: String| Diagnostic { severity: Severity::Warning, layer: None, file: None, message: m };
    for (name, value) in &doc.value {
        if name == "config_version" {
            continue;
        }
        let Some(table) = value.as_table() else {
            diags.push(warn(format!("scopes: '{name}' must be a table; ignored")));
            continue;
        };
        let mut scope = Scope::default();
        if let Some(dimensions) = table.get("dimensions").and_then(|v| v.as_table()) {
            for (column, values) in dimensions {
                let values: Vec<String> = values
                    .as_array()
                    .map(|a| a.iter().filter_map(|v| v.as_str()).map(str::to_string).collect())
                    .unwrap_or_default();
                scope.dimensions.push(DimensionSelection { column: column.clone(), values });
            }
        }
        scope.text = table.get("text").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_string);
        if let Some(src) = table.get("expression").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
            match parse_expr(src) {
                Ok(e) => scope.expression = Some(e),
                Err(e) => {
                    diags.push(warn(format!("scopes: '{name}': expression: {e}; scope ignored")));
                    continue;
                }
            }
        }
        // Validate against every dataset that has the columns; a scope
        // naming a column no dataset declares is an error for that scope.
        let bad: Vec<Diagnostic> = schema
            .datasets
            .iter()
            .map(|ds| scope.validate(ds, dims))
            .min_by_key(|d| d.len())
            .unwrap_or_default();
        if !bad.is_empty() {
            for d in bad {
                diags.push(warn(format!("scopes: '{name}': {}; scope ignored", d.message)));
            }
            continue;
        }
        out.insert(name.clone(), scope);
    }
    (out, diags)
}

/// The TOML table `persist_scope_to_user_config` writes for one scope.
pub fn scope_to_table(scope: &Scope) -> toml_edit::Table {
    let mut t = toml_edit::Table::new();
    let mut dims = toml_edit::Table::new();
    for d in &scope.dimensions {
        if d.values.is_empty() {
            continue;
        }
        let mut a = toml_edit::Array::new();
        for v in &d.values {
            a.push(v.as_str());
        }
        dims[d.column.as_str()] = toml_edit::value(a);
    }
    t["dimensions"] = toml_edit::Item::Table(dims);
    t["text"] = toml_edit::value(scope.text.clone().unwrap_or_default());
    t["expression"] = toml_edit::value(scope.expression.as_ref().map(|e| e.to_string()).unwrap_or_default());
    t
}
```

`geode-core` needs `toml_edit` as a dependency for `scope_to_table`
(check `Cargo.toml`; it has `toml` only). Add `toml_edit = "0.25.13"`.

Tests in the same file: a two-scope doc round-trips through
`saved_scopes_from_doc` → `scope_to_table` → parse → `from_doc` again
and is equal; a scope with an unknown column is dropped with a warning
naming it; a scope with a bad expression is dropped; a user layer
overriding one name replaces only that name (merge two `LayerDoc`s via
`merge_docs("scopes", …)` — the atomic list already contains `scopes`).

Run: `cargo test -p geode-core scopes 2>&1 | tail -3`

- [ ] **Step 3: Frame tests, failing**

In `frame.rs`'s `mod tests`:

```rust
    #[test]
    fn undo_and_redo_walk_a_bounded_stack() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        for i in 0..40 {
            assert!(f.set_scope(book_scope(&format!("BK{i:03}"))));
        }
        // 32 undos land on BK007 (40 sets, depth 32); a 33rd does nothing.
        for _ in 0..32 {
            assert!(f.undo_scope());
        }
        assert_eq!(f.scope().dimensions[0].values, vec!["BK007".to_string()]);
        assert!(!f.undo_scope());
        assert!(f.redo_scope());
        assert_eq!(f.scope().dimensions[0].values, vec!["BK008".to_string()]);
        // A new set clears redo.
        assert!(f.set_scope(book_scope("X")));
        assert!(!f.redo_scope());
    }

    #[test]
    fn a_no_op_set_pushes_nothing() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        assert!(f.set_scope(book_scope("A")));
        assert!(!f.set_scope(book_scope("A")));
        assert!(f.undo_scope());
        assert!(f.scope().is_empty());
        assert!(!f.undo_scope());
    }

    #[test]
    fn a_text_session_coalesces_into_one_undo_entry() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.set_scope(book_scope("A"));
        f.begin_scope_session();
        for t in ["s", "sp", "spx"] {
            let mut s = f.scope().clone();
            s.text = Some(t.into());
            assert!(f.set_scope_in_session(s));
        }
        f.end_scope_session();
        assert!(f.undo_scope());
        assert_eq!(f.scope().text, None, "one undo returns to before the session");
        assert_eq!(f.scope().dimensions[0].values, vec!["A".to_string()]);
        assert!(f.redo_scope());
        assert_eq!(f.scope().text.as_deref(), Some("spx"));
    }

    #[test]
    fn as_of_remembers_one_previous_value_in_both_directions() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        let t = chrono::Utc::now();
        assert!(f.set_as_of(AsOf::At(t)));
        assert!(f.set_as_of(AsOf::Live));
        assert!(f.undo_as_of());
        assert_eq!(f.as_of(), &AsOf::At(t));
        assert!(f.undo_as_of(), "undo swaps, so it can go back again");
        assert_eq!(f.as_of(), &AsOf::Live);
    }

    #[test]
    fn recent_publishes_keep_the_last_thirty_two_newest_first() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        let v0 = f.versions().data;
        for i in 0..40u32 {
            f.note_published(Publish {
                dataset: "risk".into(),
                batch: "EOD".into(),
                books: 3,
                at: chrono::Utc::now() + chrono::Duration::seconds(i as i64),
            });
        }
        assert_eq!(f.versions().data, v0 + 40);
        assert_eq!(f.recent_publishes().len(), RECENT_PUBLISHES);
        assert!(f.recent_publishes()[0].at > f.recent_publishes()[1].at);
    }

    #[test]
    fn saved_scopes_load_save_and_persist_pending() {
        let mut saved = SavedScopes::new();
        saved.insert("eu".into(), book_scope("BK001"));
        let mut f = Frame::new(slots(), saved, None);
        assert!(f.load_scope("eu").unwrap());
        assert_eq!(f.scope(), &book_scope("BK001"));
        assert!(f.load_scope("nope").is_err());
        f.set_scope(book_scope("BK002"));
        f.save_scope("mine").unwrap();
        assert_eq!(f.saved_scopes()["mine"], book_scope("BK002"));
        assert_eq!(f.take_pending_scope_persist(), Some(("mine".into(), book_scope("BK002"))));
        assert_eq!(f.take_pending_scope_persist(), None);
        assert!(f.save_scope("").is_err());
    }

    #[test]
    fn drop_dimension_and_set_text_are_undoable_edits() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.set_scope(book_scope("A"));
        assert!(f.drop_dimension("book"));
        assert!(f.scope().is_empty());
        assert!(!f.drop_dimension("book"));
        assert!(f.set_text(Some("spx".into())));
        assert!(!f.set_text(Some("spx".into())));
        assert!(f.undo_scope());
        assert!(f.scope().is_empty());
        assert!(f.undo_scope());
        assert_eq!(f.scope(), &book_scope("A"));
    }

    #[test]
    fn the_bar_model_is_cached_on_versions_and_describes_the_scope() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.set_active_slot(Some(1));
        let mut s = book_scope("BK001");
        s.dimensions[0].values.push("BK002".into());
        s.dimensions.push(DimensionSelection { column: "lhu".into(), values: (0..7).map(|i| i.to_string()).collect() });
        s.text = Some("spx".into());
        s.expression = Some(geode_core::scope::parse_expr("npv > 0").unwrap());
        f.set_scope(s);
        let m1 = f.bar_model();
        let m2 = f.bar_model();
        assert!(Rc::ptr_eq(&m1, &m2));
        assert_eq!(m1.slot, Some((1, "book / lhu".into())));
        assert_eq!(m1.chips[0].summary, "book ∈ BK001, BK002");
        assert_eq!(m1.chips[1].summary, "lhu ∈ {7}");
        assert_eq!(m1.text.as_deref(), Some("spx"));
        assert_eq!(m1.expr.as_deref(), Some("npv > 0"));
        assert_eq!(m1.as_of, None);
        f.set_text(None);
        assert!(!Rc::ptr_eq(&m1, &f.bar_model()));
    }

    #[test]
    fn a_contradiction_is_named_not_hidden() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        let s = book_scope("A").and_then(&book_scope("B"));
        assert!(s.impossible);
        f.set_scope(s);
        assert_eq!(f.bar_model().impossible.as_deref(), Some("∅ book"));
    }
```

`persist_scope_to_user_config` test: write to a `tempfile::tempdir`,
then read the file back through `toml_edit` and `saved_scopes_from_doc`
and assert the scope is there and a pre-existing sibling key and a
comment survived (mirror the `persist_slot_to_user_config` tests in the
same module).

Run: `cargo test -p geode-shell frame 2>&1 | tail -5`
Expected: compile errors.

- [ ] **Step 4: Implement the frame changes**

Replace `previous_scope` with the stacks and add the fields:

```rust
pub struct Frame {
    scope: Scope,
    scope_undo: Vec<Scope>,
    scope_redo: Vec<Scope>,
    /// `Some` while the text field has focus: the first mutation of the
    /// session pushes this, later ones push nothing (spec §3.8).
    scope_session: Option<Option<Scope>>,   // None: no session; Some(Some(base)): open, unpushed; Some(None): open, pushed
    slots: GroupingSlots,
    active_slot: Option<u8>,
    as_of: AsOf,
    previous_as_of: Option<AsOf>,
    recent_publishes: VecDeque<Publish>,
    saved_scopes: SavedScopes,
    pending_scope_persist: Option<(String, Scope)>,
    versions: FrameVersions,
    pub requery: RequeryStats,
    user_dir: Option<PathBuf>,
    pending_persist: Option<(u8, Vec<String>)>,
    bar_cache: RefCell<Option<(FrameVersions, Rc<ScopeBarModel>)>>,
}
```

Core mutations:

```rust
    fn push_undo(&mut self, outgoing: Scope) {
        self.scope_undo.push(outgoing);
        if self.scope_undo.len() > UNDO_DEPTH {
            self.scope_undo.remove(0);
        }
        self.scope_redo.clear();
    }

    pub fn set_scope(&mut self, scope: Scope) -> bool {
        if self.scope == scope {
            return false;
        }
        let outgoing = std::mem::replace(&mut self.scope, scope);
        self.push_undo(outgoing);
        self.versions.scope += 1;
        true
    }

    pub fn begin_scope_session(&mut self) {
        self.scope_session = Some(Some(self.scope.clone()));
    }

    pub fn set_scope_in_session(&mut self, scope: Scope) -> bool {
        if self.scope == scope {
            return false;
        }
        match self.scope_session.take() {
            Some(Some(base)) => {
                self.push_undo(base);
                self.scope_session = Some(None);
            }
            Some(None) => self.scope_session = Some(None),
            None => {
                // No session open: behave as set_scope.
                let outgoing = self.scope.clone();
                self.push_undo(outgoing);
            }
        }
        self.scope = scope;
        self.versions.scope += 1;
        true
    }

    pub fn end_scope_session(&mut self) {
        self.scope_session = None;
    }

    pub fn undo_scope(&mut self) -> bool {
        let Some(previous) = self.scope_undo.pop() else { return false; };
        let current = std::mem::replace(&mut self.scope, previous);
        self.scope_redo.push(current);
        self.versions.scope += 1;
        true
    }

    pub fn redo_scope(&mut self) -> bool {
        let Some(next) = self.scope_redo.pop() else { return false; };
        let current = std::mem::replace(&mut self.scope, next);
        self.scope_undo.push(current);
        self.versions.scope += 1;
        true
    }

    pub fn drop_dimension(&mut self, column: &str) -> bool {
        let mut s = self.scope.clone();
        let before = s.dimensions.len();
        s.dimensions.retain(|d| d.column != column);
        if s.dimensions.len() == before {
            return false;
        }
        self.set_scope(s)
    }

    pub fn set_text(&mut self, text: Option<String>) -> bool {
        let mut s = self.scope.clone();
        s.text = text.filter(|t| !t.trim().is_empty());
        self.set_scope(s)
    }

    pub fn set_as_of(&mut self, as_of: AsOf) -> bool {
        if self.as_of == as_of {
            return false;
        }
        self.previous_as_of = Some(std::mem::replace(&mut self.as_of, as_of));
        self.versions.as_of += 1;
        true
    }

    pub fn undo_as_of(&mut self) -> bool {
        let Some(previous) = self.previous_as_of.take() else { return false; };
        let current = std::mem::replace(&mut self.as_of, previous);
        self.previous_as_of = Some(current);
        self.versions.as_of += 1;
        true
    }

    pub fn note_published(&mut self, publish: Publish) {
        self.recent_publishes.push_front(publish);
        self.recent_publishes.truncate(RECENT_PUBLISHES);
        self.versions.data += 1;
    }

    pub fn save_scope(&mut self, name: &str) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() || name == "config_version" || name.contains(|c: char| c.is_whitespace() || c == '.' || c == '"') {
            return Err(format!("'{name}' is not a usable scope name"));
        }
        self.saved_scopes.insert(name.to_string(), self.scope.clone());
        self.pending_scope_persist = Some((name.to_string(), self.scope.clone()));
        self.versions.config += 1;
        Ok(())
    }

    pub fn load_scope(&mut self, name: &str) -> Result<bool, String> {
        let scope = self.saved_scopes.get(name).cloned().ok_or_else(|| format!("no saved scope '{name}'"))?;
        Ok(self.set_scope(scope))
    }

    pub fn replace_saved_scopes(&mut self, saved: SavedScopes) -> bool {
        if self.saved_scopes == saved { return false; }
        self.saved_scopes = saved;
        self.versions.config += 1;
        true
    }
```

`bar_model` mirrors `readout`'s cache exactly, calling
`scopebar::build_model(self, chrono::Local::now())`. Delete `readout`,
`FrameReadout`, `build_readout` and `readout_cache`; update
`toolbar::toolbar` to take `&ScopeBarModel` and render the same three
things it does today from the model's fields (chips are Task 4; here the
chips render as today's joined summary text so the toolbar keeps
painting), `render.rs:766` to call `bar_model`, and every test in
`shell/tests/` that asserted on `readout()` to assert on the model.

`persist_scope_to_user_config` is `persist_slot_to_user_config` with
`scopes.toml`, `doc[name] = toml_edit::Item::Table(scope_to_table(scope))`,
and the same `config_version` seeding. `ShellView::on_frame_changed`
drains `take_pending_scope_persist` beside `take_pending_persist` and
writes on the background executor with the same `eprintln!` on failure
(4b migrates it).

`scopebar.rs`:

```rust
pub fn build_model(frame: &Frame, now: DateTime<Local>) -> ScopeBarModel {
    let scope = frame.scope();
    let slot = frame.active_slot().and_then(|n| frame.slots().label(n).map(|l| (n, l)));
    let chips = scope
        .dimensions
        .iter()
        .filter(|d| !d.values.is_empty())
        .map(|d| Chip {
            column: d.column.clone(),
            summary: if d.values.len() <= 2 {
                format!("{} ∈ {}", d.column, d.values.join(", "))
            } else {
                format!("{} ∈ {{{}}}", d.column, d.values.len())
            },
        })
        .collect();
    let expr = scope.expression.as_ref().map(|e| {
        let s = e.to_string();
        if s.chars().count() > 40 {
            format!("{}…", s.chars().take(40).collect::<String>())
        } else {
            s
        }
    });
    let impossible = scope.impossible.then(|| {
        let named = scope.columns().into_iter().next().unwrap_or_default();
        format!("∅ {named}")
    });
    let as_of = match frame.as_of() {
        AsOf::Live => None,
        AsOf::At(t) => {
            let local = t.with_timezone(&Local);
            Some(if local.date_naive() == now.date_naive() {
                local.format("%H:%M").to_string()
            } else {
                local.format("%Y-%m-%d %H:%M").to_string()
            })
        }
    };
    ScopeBarModel { slot, chips, text: scope.text.clone(), expr, impossible, as_of }
}
```

`Frame::new` gains `saved: SavedScopes`; update the three constructors
in the workspace (`ShellView::new` via `rebuild_slots`'s neighbour — add
`rebuild_saved_scopes(&Config) -> SavedScopes` in `hot_reload.rs`
reading the `scopes` doc with the schema and dims the way
`rebuild_slots` does; the blotter's test `Frame::new(GroupingSlots::default(), None)`
becomes `Frame::new(GroupingSlots::default(), SavedScopes::new(), None)`;
the bridge's tests likewise).

`hot_reload::apply_reload`: `let scopes_changed = changed("scopes") ||
changed("datasets") || changed("dimensions");` → `replace_saved_scopes`
with a notify, placed after the `groupings_changed` block.

`bridge.rs`: the `Published` arm passes
`Publish { dataset, batch, books: books.len(), at: Utc::now() }`. (The
event carries no timestamp; the arrival instant is what a preset needs.)

- [ ] **Step 5: Session `[frame]`**

`session.rs`:

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct FrameRecord {
    pub scope: Scope,
    pub active_slot: Option<u8>,
    pub as_of: AsOf,
}

impl FrameRecord {
    pub fn to_toml(&self) -> toml::Table {
        let mut t = toml::Table::new();
        let mut dims = toml::Table::new();
        for d in self.scope.dimensions.iter().filter(|d| !d.values.is_empty()) {
            dims.insert(d.column.clone(), toml::Value::Array(d.values.iter().map(|v| toml::Value::String(v.clone())).collect()));
        }
        t.insert("dimensions".into(), toml::Value::Table(dims));
        if let Some(text) = &self.scope.text {
            t.insert("text".into(), toml::Value::String(text.clone()));
        }
        if let Some(e) = &self.scope.expression {
            t.insert("expression".into(), toml::Value::String(e.to_string()));
        }
        if let Some(n) = self.active_slot {
            t.insert("slot".into(), toml::Value::Integer(n as i64));
        }
        if let AsOf::At(at) = &self.as_of {
            t.insert("as_of".into(), toml::Value::String(at.to_rfc3339()));
        }
        t
    }

    /// Lenient: a field that does not parse is dropped with a warning, the
    /// rest of the record survives.
    pub fn from_toml(t: &toml::Table, warnings: &mut Vec<String>) -> FrameRecord { /* inverse */ }
}
```

`to_toml`/`to_string_pretty`/`save` take `frame: Option<&FrameRecord>`
and write `[frame]` when `Some`; `from_toml` fills `Restored.frame`.
Update every caller (tests pass `None`). `take_dirty_session_write`
builds the record from `self.frame.read(cx)` and treats a change in
`(scope, grouping, as_of)` versions since `last_frame_versions_written`
as dirty. `ShellServices` gains `restored_frame: Option<FrameRecord>`
(`main.rs` sets it from `restored.frame`; `test_services` sets `None`),
and `ShellView::new` applies it to the frame after construction with
`set_scope`, `set_active_slot`, `set_as_of` — then clears the undo stack
it just pushed onto (`frame.clear_history()`, a new method that empties
both stacks), so a restored session does not start with a phantom undo
entry.

Tests: `session.rs` round-trips a `FrameRecord` with every field; a
record with a bad `as_of` keeps the scope and warns; `shell/tests/session.rs`
opens a shell with `restored_frame: Some(..)` and asserts the frame's
scope and as-of, and that the first flush writes `[frame]`.

- [ ] **Step 6: Undo/redo/clear actions and saved scopes in the palette**

`defaults.rs`: register `frame::scope_undo` ("Undo scope change",
"Frame"), `frame::scope_redo` ("Redo scope change", "Frame"),
`frame::scope_clear` ("Clear scope", "Frame"); bind `"mod+z" =
"frame::scope_undo"` and `"mod+shift+z" = "frame::scope_redo"` in the
shell context of `BUILTIN_KEYMAP`. `input.rs` arms:

```rust
        } else if action.0 == "frame::scope_undo" {
            self.frame.update(cx, |f, cx| { if f.undo_scope() { cx.notify(); } });
        } else if action.0 == "frame::scope_redo" {
            self.frame.update(cx, |f, cx| { if f.redo_scope() { cx.notify(); } });
        } else if action.0 == "frame::scope_clear" {
            self.frame.update(cx, |f, cx| { if f.clear_scope() { cx.notify(); } });
```

Palette: `PaletteItem::Scope(String)` with `title()` = `Scope: {name}`,
`category()` = `"Scope"`, `binding()` = `None`. `build_items` gains a
`saved: &SavedScopes` parameter and appends one item per key after the
theme rows; `toggle_palette` passes `self.frame.read(cx).saved_scopes()`.
`dispatch_palette_item` arm:

```rust
            PaletteItem::Scope(name) => {
                let name = name.clone();
                self.frame.update(cx, |f, cx| {
                    if let Ok(true) = f.load_scope(&name) {
                        cx.notify();
                    }
                });
            }
```

Tests: in `tests/palette.rs`, a frame with a saved scope `eu` lists
`Scope: eu` and selecting it sets the frame's scope (assert
`frame.scope()` equals the saved one and `versions().scope` bumped
once); in `tests/scopebar.rs` (Task 4 creates the file — add this test
there in Task 4, or create the file now with just this test and let
Task 4 extend it): after `frame.set_scope(a)` then `set_scope(b)`,
`ctrl-z` restores `a` and `ctrl-shift-z` returns to `b`.

Harness entries for this step (append to Step 8's list):

```sh
run_mutation "palette: selecting a saved scope loads it" \
  crates/geode-shell/src/shell/palette_ctl.rs \
  '                    if let Ok(true) = f.load_scope(&name) {' \
  '                    if let Ok(true) = f.load_scope("no-such-scope") {' \
  geode-shell <the tests/palette.rs saved-scope test>
```

- [ ] **Step 7: Run everything**

Run: `cargo test --workspace 2>&1 | tail -3 && cargo clippy --workspace --all-targets -- -D warnings`

- [ ] **Step 8: Harness entries**

```sh
run_mutation "frame: undo is bounded" \
  crates/geode-shell/src/frame.rs \
  '        if self.scope_undo.len() > UNDO_DEPTH {' \
  '        if false {' \
  geode-shell undo_and_redo_walk_a_bounded_stack

run_mutation "frame: a new set clears redo" \
  crates/geode-shell/src/frame.rs \
  '        self.scope_redo.clear();' \
  '        let _ = &self.scope_redo;' \
  geode-shell undo_and_redo_walk_a_bounded_stack

run_mutation "frame: a text session pushes once" \
  crates/geode-shell/src/frame.rs \
  '            Some(None) => self.scope_session = Some(None),' \
  '            Some(None) => { let o = self.scope.clone(); self.push_undo(o); self.scope_session = Some(None) }' \
  geode-shell a_text_session_coalesces_into_one_undo_entry

run_mutation "frame: as-of undo swaps rather than consumes" \
  crates/geode-shell/src/frame.rs \
  '        self.previous_as_of = Some(current);' \
  '        let _ = current;' \
  geode-shell as_of_remembers_one_previous_value_in_both_directions

run_mutation "frame: recent publishes are bounded and newest first" \
  crates/geode-shell/src/frame.rs \
  '        self.recent_publishes.push_front(publish);' \
  '        self.recent_publishes.push_back(publish);' \
  geode-shell recent_publishes_keep_the_last_thirty_two_newest_first

run_mutation "frame: the bar names a contradiction" \
  crates/geode-shell/src/scopebar.rs \
  '    let impossible = scope.impossible.then(|| {' \
  '    let impossible = false.then(|| {' \
  geode-shell a_contradiction_is_named_not_hidden

run_mutation "scopes: an unknown column drops the scope" \
  crates/geode-core/src/scopes.rs \
  '        if !bad.is_empty() {' \
  '        if false {' \
  geode-core <the unknown-column scopes test>

run_mutation "session: [frame] restores as-of" \
  crates/geode-shell/src/session.rs \
  '            t.insert("as_of".into(), toml::Value::String(at.to_rfc3339()));' \
  '            let _ = at;' \
  geode-shell <the FrameRecord round-trip test>
```

Run: `zsh scripts/mutation-check.sh --changed`, then CI checks.

```bash
git add -A
git commit -m "feat(frame): undo/redo stacks, previous as-of, recent publishes, saved scopes, the bar model, session [frame] (Phase 4 §3.6-3.9, §3.12)"
```

---

### Task 4: The scope bar and the live text field

Spec §3.1, §3.2, §3.11 (`frame::focus_text`, `mod+/`). Chips with close
glyphs, the field wired to the frame per keystroke, Enter/Escape
semantics, and the frame's text reflected back into the field.

**Files:**
- Modify: `crates/geode-shell/src/shell/toolbar.rs`
- Modify: `crates/geode-shell/src/shell/mod.rs` (subscription on `filter_input`, `filter_session_base: Option<String>`, `on_frame_changed` reflection)
- Modify: `crates/geode-shell/src/shell/input.rs` (escape branch; `frame::focus_text` arm)
- Modify: `crates/geode-shell/src/defaults.rs` (`frame::focus_text`, `"mod+/"`)
- Create: `crates/geode-shell/src/shell/tests/scopebar.rs`; register in `tests/mod.rs`
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Consumes: `Frame::{bar_model, begin_scope_session, set_scope_in_session, end_scope_session, drop_dimension, set_text}`, `ScopeBarModel`, `InputEvent`.
- Produces: `toolbar::toolbar(filter_input, model: &ScopeBarModel, on_chip_close: impl Fn(&str, &mut Window, &mut App) + Clone + 'static, cx)`; `ShellView::focus_text_field(window, cx)`; debug selectors `scope-chip-<column>`, `scope-chip-close-<column>`, `scope-text-chip`, `scope-expr-chip`, `scope-impossible-chip`, `scope-asof`.

- [ ] **Step 1: Check `InputEvent`'s variants against the pinned checkout**

Run: `grep -n "pub enum InputEvent" -A12 ~/.cargo/git/checkouts/gpui-component-95ce574d8a0da8b8/0e2fb7a/crates/base/src/input/base/*.rs ~/.cargo/git/checkouts/gpui-component-95ce574d8a0da8b8/0e2fb7a/crates/ui/src/input/*.rs 2>/dev/null | head -20`

Expected: `Change`, `PressEnter { secondary: bool }`, `Focus`, `Blur`.
If `Focus`/`Blur` are absent, focus transitions are detected instead in
`handle_key_down`'s existing `filter_input … is_focused(window)` branch
plus a `focus_handle.is_focused` compare in `render` (cheap: a bool),
storing `filter_had_focus: bool` on `ShellView` and calling
`begin_scope_session`/`end_scope_session` on the transition.

- [ ] **Step 2: Failing e2e tests**

`crates/geode-shell/src/shell/tests/scopebar.rs`:

```rust
use super::*;

#[gpui::test]
fn typing_in_the_field_sets_the_frame_text_per_keystroke_and_enter_blurs(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-/");   // mod+/ under the default mod
    assert!(filter_is_focused(&shell, &mut vcx));
    vcx.simulate_input("sp");
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    assert_eq!(frame.read_with(&vcx, |f, _| f.scope().text.clone()).as_deref(), Some("sp"));
    let v_after_two = frame.read_with(&vcx, |f, _| f.versions().scope);
    vcx.simulate_input("x");
    assert_eq!(frame.read_with(&vcx, |f, _| f.versions().scope), v_after_two + 1);
    vcx.simulate_keystrokes("enter");
    assert!(!filter_is_focused(&shell, &mut vcx));
    assert_eq!(frame.read_with(&vcx, |f, _| f.scope().text.clone()).as_deref(), Some("spx"));
    // One undo entry for the whole session.
    frame.update(&mut vcx, |f, _| assert!(f.undo_scope()));
    assert_eq!(frame.read_with(&vcx, |f, _| f.scope().text.clone()), None);
}

#[gpui::test]
fn escape_restores_the_text_the_field_had_when_focused(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    frame.update(&mut vcx, |f, _| { f.set_text(Some("old".into())); });
    vcx.simulate_keystrokes("ctrl-/");
    vcx.simulate_input("new");
    vcx.simulate_keystrokes("escape");
    assert!(!filter_is_focused(&shell, &mut vcx));
    assert_eq!(frame.read_with(&vcx, |f, _| f.scope().text.clone()).as_deref(), Some("old"));
    let value = shell.read_with(&vcx, |s, cx| s.filter_input.read(cx).value().to_string());
    assert_eq!(value, "old");
}

#[gpui::test]
fn a_text_set_elsewhere_shows_in_the_field_and_a_chip_close_drops_the_dimension(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    frame.update(&mut vcx, |f, cx| {
        let mut s = f.scope().clone();
        s.text = Some("from-tile".into());
        s.dimensions.push(geode_core::scope::DimensionSelection { column: "book".into(), values: vec!["BK001".into()] });
        f.set_scope(s);
        cx.notify();
    });
    vcx.run_until_parked();
    let value = shell.read_with(&vcx, |s, cx| s.filter_input.read(cx).value().to_string());
    assert_eq!(value, "from-tile");
    let close = vcx.debug_bounds("scope-chip-close-book").expect("chip painted");
    vcx.simulate_click(close.center(), gpui::Modifiers::default());
    assert!(frame.read_with(&vcx, |f, _| f.scope().dimensions.is_empty()));
    assert!(vcx.debug_bounds("scope-chip-close-book").is_none());
}
```

Add `pub(super) fn filter_is_focused` if it does not already fit
(`tests/mod.rs:171` has one; reuse it).

Run: `cargo test -p geode-shell --features test-support scopebar 2>&1 | tail -5`
Expected: FAIL (`ctrl-/` unbound; no chip selectors).

- [ ] **Step 3: Implement**

`defaults.rs`: register `frame::focus_text` ("Focus the scope text
field", "Frame") and bind `"mod+/" = "frame::focus_text"` in the shell
context block of `BUILTIN_KEYMAP`. `input.rs`: dispatch arm
`"frame::focus_text" => self.focus_text_field(window, cx)`, where

```rust
    pub(super) fn focus_text_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = self.filter_input.read(cx).focus_handle(cx);
        handle.focus(window, cx);
        cx.notify();
    }
```

In `ShellView::new`, subscribe:

```rust
        cx.subscribe_in(&filter_input, window, |view, input, event, window, cx| {
            match event {
                InputEvent::Focus => {
                    view.filter_session_base = Some(input.read(cx).value().to_string());
                    view.frame.update(cx, |f, _| f.begin_scope_session());
                }
                InputEvent::Change => {
                    let text = input.read(cx).value().to_string();
                    view.frame.update(cx, |f, cx| {
                        let mut s = f.scope().clone();
                        s.text = (!text.trim().is_empty()).then_some(text);
                        if f.set_scope_in_session(s) {
                            cx.notify();
                        }
                    });
                }
                InputEvent::PressEnter { .. } => {
                    view.filter_session_base = None;
                    view.frame.update(cx, |f, _| f.end_scope_session());
                    view.focus_handle.focus(window, cx);
                    cx.notify();
                }
                InputEvent::Blur => {
                    view.filter_session_base = None;
                    view.frame.update(cx, |f, _| f.end_scope_session());
                }
                _ => {}
            }
        })
        .detach();
```

`input.rs`, the existing escape branch for the focused filter: before
focusing the shell, restore:

```rust
            if event.keystroke.key == "escape" {
                if let Some(base) = self.filter_session_base.take() {
                    self.filter_input.update(cx, |i, cx| i.set_value(base.clone(), window, cx));
                    self.frame.update(cx, |f, cx| {
                        f.end_scope_session();
                        if f.set_text((!base.trim().is_empty()).then_some(base)) {
                            cx.notify();
                        }
                    });
                }
                self.focus_handle.focus(window, cx);
                cx.notify();
            }
```

(`set_value` emits no `Change`, per the checked note in `dialog.rs`.
`set_text` after `end_scope_session` pushes one undo entry for the
revert — acceptable: undo then goes to the session's start, which is the
same scope, and a second undo goes before it. Simpler than special-
casing.)

`on_frame_changed` in `mod.rs`: when the field is not focused and its
value differs from `frame.scope().text`, `set_value` it (an unfocused
field always shows the frame's truth). Reading the value is a
`SharedString` clone per frame notify, not per render — fine.

`toolbar.rs`: render the model. Chips:

```rust
fn chip(label: String, fg: Hsla, bg: Hsla, selector: String) -> Div {
    div()
        .px_2()
        .py_0p5()
        .rounded(px(4.))
        .bg(bg)
        .text_color(fg)
        .child(label)
        .debug_selector(move || selector.clone())
}
```

For each `Chip` in `model.chips`: an `h_flex` with the chip body
(`scope-chip-{column}`) and a close glyph (`Icon::new(IconName::Close)`
in a `div` with `debug_selector("scope-chip-close-{column}")` and
`on_mouse_down(MouseButton::Left, { let col = chip.column.clone(); let f = on_chip_close.clone(); move |_, w, cx| f(&col, w, cx) })`).
Text chip (`scope-text-chip`, `text "spx"`), expr chip
(`scope-expr-chip`), impossible chip (`scope-impossible-chip`, error
tokens: `theme.danger` bg at 0.25, `theme.danger_foreground`), and the
as-of marker (`scope-asof`) with the existing warning treatment on the
whole bar. `render.rs` passes
`cx.listener(|view, column: &str, window, cx| { view.frame.update(cx, |f, cx| { if f.drop_dimension(column) { cx.notify(); } }); })`
— adapt to the closure shape `on_chip_close` expects (build the closure
in `render` capturing `cx.entity()` and calling `entity.update`).

- [ ] **Step 4: Run, harness, commit**

Run: `cargo test -p geode-shell --features test-support 2>&1 | tail -3` and CI checks.

```sh
run_mutation "bar: a keystroke sets the frame text" \
  crates/geode-shell/src/shell/mod.rs \
  '                        if f.set_scope_in_session(s) {' \
  '                        if false && f.set_scope_in_session(s) {' \
  geode-shell typing_in_the_field_sets_the_frame_text_per_keystroke_and_enter_blurs

run_mutation "bar: escape restores the pre-focus text" \
  crates/geode-shell/src/shell/input.rs \
  '                if let Some(base) = self.filter_session_base.take() {' \
  '                if let Some(base) = self.filter_session_base.take().filter(|_| false) {' \
  geode-shell escape_restores_the_text_the_field_had_when_focused

run_mutation "bar: a chip close drops the dimension" \
  crates/geode-shell/src/shell/render.rs \
  '                    if f.drop_dimension(column) {' \
  '                    if false {' \
  geode-shell a_text_set_elsewhere_shows_in_the_field_and_a_chip_close_drops_the_dimension
```

```bash
git add -A
git commit -m "feat(shell): the scope bar — chips with close glyphs and a live per-keystroke text field (Phase 4 §3.1-3.2)"
```

---

### Task 5: Dimension pickers

Spec §3.3 (the pickers), §3.4 (delivery), §3.11 (`frame::pick`,
`frame::pick_<column>`). A two-stage keyed modal in the keybinding
dialog's mould, fed by `Request::Distinct` through the bridge.

Two key-binding amendments to the spec, recorded here and to be copied
into spec §3.3 in Task 9: values are toggled with `tab` (a printable
`space` would be typed into the filter input; `tab` is already reclaimed
to `NoAction` inside every Geode modal), and "clear all" is `ctrl+x`
(`ctrl+n` is "down" on every list surface in the shell). `ctrl+a`
"select all shown" needs `ctrl-a` reclaimed inside `GeodeModal` the way
`tab` is; add it to `init_reclaimed_keybindings` with a comment in the
same style as its three existing bullets.

**Files:**
- Create: `crates/geode-shell/src/shell/picker.rs`
- Modify: `crates/geode-shell/src/shell/mod.rs` (`picker: Option<PickerState>`, `pickable: Vec<Pickable>`, `deliver_distinct`, `ShellEvent::DistinctRequested`, `PICKER_KEY`)
- Modify: `crates/geode-shell/src/shell/dialog.rs` (`ctrl-a` reclaim)
- Modify: `crates/geode-shell/src/shell/input.rs` (`frame::pick`, `frame::pick_*` arms)
- Modify: `crates/geode-shell/src/shell/hot_reload.rs` (rebuild `pickable` on `datasets`/`dimensions`)
- Modify: `crates/geode-shell/src/shell/toolbar.rs`, `render.rs` (chip body click opens the picker)
- Modify: `crates/geode-shell/src/defaults.rs` (`frame::pick`, `mod+p`, `register_pick_actions`)
- Modify: `crates/geode-app/src/main.rs` (call `register_pick_actions`), `bridge.rs` (both directions)
- Create: `crates/geode-shell/src/shell/tests/picker.rs`
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Consumes: `DistinctParams`, `DistinctOutcome`, `QueryKey`, `Frame::{scope, set_scope, as_of}`, `palette::fuzzy_match`, `open_shell_dialog_with_key`, `filter_row`.
- Produces:
  ```rust
  // geode_shell::shell::mod
  pub const PICKER_KEY: QueryKey = QueryKey(u64::MAX - 1);
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct Pickable { pub column: String, pub role: &'static str /* "dimension" | "attribute" | "derived" */, pub datasets: Vec<String> }
  pub fn pickable_columns(config: &Config) -> Vec<Pickable>;
  pub enum ShellEvent { ConfigReloaded, RestartRequired(String), DistinctRequested(DistinctParams) }
  impl ShellView { pub fn deliver_distinct(&mut self, outcome: DistinctOutcome, cx: &mut Context<Self>); }
  // geode_shell::shell::picker (pure core)
  pub enum Stage { Columns, Values { column: String } }
  pub struct PickerState {
      pub stage: Stage,
      pub selected: usize,
      pub query: String,
      pub values: Option<Result<Vec<(String, u64)>, String>>,   // None: loading
      pub ticked: BTreeSet<String>,
      pub tag: u64,
  }
  impl PickerState {
      pub fn columns(pickable: &[Pickable], query: &str) -> Vec<(usize, Vec<usize>)>;   // filtered indices + match positions
      pub fn shown(&self) -> Vec<(usize, Vec<usize>)>;                                  // filtered value indices
      pub fn toggle_selected(&mut self);
      pub fn tick_all_shown(&mut self);
      pub fn clear(&mut self);
      pub fn move_selection(&mut self, delta: i32, len: usize);
      pub fn apply(&self, scope: &Scope) -> Scope;   // replaces this column's selection; empty ticked drops it
  }
  pub fn open(view: &mut ShellView, column: Option<String>, window: &mut Window, cx: &mut Context<ShellView>);
  // geode_shell::defaults
  pub fn register_pick_actions(reg: &mut ActionRegistry, columns: &[Pickable]);
  ```

- [ ] **Step 1: Pure-core tests, failing**

`picker.rs` `mod tests`:

```rust
    fn state_with(values: &[(&str, u64)]) -> PickerState {
        PickerState {
            stage: Stage::Values { column: "book".into() },
            selected: 0,
            query: String::new(),
            values: Some(Ok(values.iter().map(|(v, n)| (v.to_string(), *n)).collect())),
            ticked: BTreeSet::new(),
            tag: 1,
        }
    }

    #[test]
    fn toggling_ticks_and_unticks_the_selected_shown_value() {
        let mut s = state_with(&[("BK000", 1), ("BK001", 2), ("XX", 3)]);
        s.query = "bk".into();
        assert_eq!(s.shown().len(), 2);
        s.selected = 1;
        s.toggle_selected();
        assert!(s.ticked.contains("BK001"));
        s.toggle_selected();
        assert!(s.ticked.is_empty());
    }

    #[test]
    fn tick_all_shown_respects_the_filter_and_clear_empties() {
        let mut s = state_with(&[("BK000", 1), ("BK001", 2), ("XX", 3)]);
        s.query = "bk".into();
        s.tick_all_shown();
        assert_eq!(s.ticked.iter().cloned().collect::<Vec<_>>(), vec!["BK000", "BK001"]);
        s.clear();
        assert!(s.ticked.is_empty());
    }

    #[test]
    fn apply_replaces_the_columns_selection_and_an_empty_tick_set_drops_it() {
        let mut s = state_with(&[("BK000", 1), ("BK001", 2)]);
        let base = Scope {
            dimensions: vec![
                DimensionSelection { column: "lhu".into(), values: vec!["A".into()] },
                DimensionSelection { column: "book".into(), values: vec!["OLD".into()] },
            ],
            ..Scope::default()
        };
        s.ticked.insert("BK001".into());
        let out = s.apply(&base);
        assert_eq!(out.dimensions.len(), 2);
        assert_eq!(out.dimensions[1].values, vec!["BK001".to_string()]);
        s.clear();
        let out = s.apply(&base);
        assert_eq!(out.dimensions.len(), 1, "book dropped");
        assert_eq!(out.dimensions[0].column, "lhu");
    }

    #[test]
    fn columns_stage_filters_by_fuzzy_match_and_keeps_schema_order_on_empty_query() {
        let p = vec![
            Pickable { column: "book".into(), role: "dimension", datasets: vec!["risk".into()] },
            Pickable { column: "currency".into(), role: "dimension", datasets: vec!["risk".into()] },
            Pickable { column: "desk".into(), role: "derived", datasets: vec![] },
        ];
        assert_eq!(PickerState::columns(&p, "").iter().map(|(i, _)| *i).collect::<Vec<_>>(), vec![0, 1, 2]);
        assert_eq!(PickerState::columns(&p, "cur").iter().map(|(i, _)| *i).collect::<Vec<_>>(), vec![1]);
    }

    #[test]
    fn pickable_columns_are_every_categorical_column_plus_derived_dimensions() {
        let config = config_from(&[("datasets", CARRIED_DEMO), ("dimensions", "desk = { from = \"book\", values = { BK000 = \"Flow\" } }")]);
        let p = pickable_columns(&config);
        let names: Vec<&str> = p.iter().map(|c| c.column.as_str()).collect();
        assert_eq!(names, vec!["book", "lhu", "counterparty", "underlying_ref", "currency", "expiry", "desk"]);
        assert_eq!(p[6].role, "derived");
        assert!(!names.contains(&"position_ref"), "keys are not categorical");
    }
```

`config_from` builds a `Config` from builtin `LayerDoc`s (`Config::load`
over `ConfigSources { builtin: vec![..], ..Default::default() }`) —
check `tests/mod.rs::config_with_mod` for the idiom.

- [ ] **Step 2: Implement the pure core and `pickable_columns`**

`PickerState::shown` runs `fuzzy_match(&self.query, value)` over the
value list, keeps `Some`, and sorts by descending score then value for a
non-empty query (identical to `PaletteState::filtered`'s rule), schema
order for an empty one. `columns` does the same over `Pickable.column`.
`apply`:

```rust
    pub fn apply(&self, scope: &Scope) -> Scope {
        let Stage::Values { column } = &self.stage else { return scope.clone(); };
        let mut out = scope.clone();
        out.dimensions.retain(|d| &d.column != column);
        if !self.ticked.is_empty() {
            out.dimensions.push(DimensionSelection {
                column: column.clone(),
                values: self.ticked.iter().cloned().collect(),
            });
        }
        out
    }
```

`pickable_columns(config)` in `shell/mod.rs`: parse the `datasets` doc
with `SchemaSpec::from_doc` and `dimensions` with
`DerivedDimensions::from_doc`; for every dataset, every
`categorical_columns()` entry becomes/extends a `Pickable` (first-seen
order, datasets appended), role `"dimension"` for `ColumnRole::Dimension`
and `"attribute"` otherwise; then every derived dimension as
`"derived"`. Stored on `ShellView.pickable` at construction and rebuilt
in `apply_reload` when `changed("datasets") || changed("dimensions")`.

- [ ] **Step 3: Actions**

`defaults.rs`:

```rust
/// `frame::pick_<column>` for every pickable column (Phase 4 §3.3).
/// Registered at startup from the loaded schema, so the palette lists
/// `Pick: book`, and a keymap can bind `mod+b = "frame::pick_book"`.
pub fn register_pick_actions(reg: &mut ActionRegistry, columns: &[Pickable]) {
    for c in columns {
        action(reg, &format!("frame::pick_{}", c.column), &format!("Pick: {}", c.column), "Frame");
    }
}
```

plus `frame::pick` ("Pick a dimension", "Frame") in
`register_builtin_actions` and `"mod+p" = "frame::pick"` in the keymap.
`main.rs` calls `register_pick_actions(&mut registry, &pickable_columns(&config))`
right after `register_builtin_actions` and before `build_keymap`;
`test_services` does the same over its (empty) config so the pattern is
exercised, and `services_with_recorder`-style helpers in the picker
tests build a config with the `CARRIED_DEMO` datasets doc.

`input.rs` arms:

```rust
        } else if action.0 == "frame::pick" {
            picker::open(self, None, window, cx);
        } else if let Some(column) = action.0.strip_prefix("frame::pick_") {
            picker::open(self, Some(column.to_string()), window, cx);
        }
```

- [ ] **Step 4: The modal**

`picker::open`:

```rust
pub fn open(view: &mut ShellView, column: Option<String>, window: &mut Window, cx: &mut Context<ShellView>) {
    if view.modal.is_some() {
        return;
    }
    let stage = match column {
        Some(c) if view.pickable.iter().any(|p| p.column == c) => Stage::Values { column: c },
        Some(_) | None => Stage::Columns,
    };
    view.picker = Some(PickerState { stage, selected: 0, query: String::new(), values: None, ticked: BTreeSet::new(), tag: 0 });
    if let Some(Stage::Values { column }) = view.picker.as_ref().map(|p| p.stage.clone()) {
        request_values(view, &column, cx);
    }
    let entity = cx.entity();
    dialog::open_shell_dialog_with_key(
        view, window, cx, "Pick",
        move |shell, window, cx| build(shell, &entity, window, cx),
        Some(Rc::new(handle_key)),
        true,
    );
}

fn request_values(view: &mut ShellView, column: &str, cx: &mut Context<ShellView>) {
    let Some(p) = view.picker.as_mut() else { return; };
    p.tag += 1;
    p.values = None;
    // Pre-tick the current selection.
    let (scope, as_of) = {
        let f = view.frame.read(cx);
        (f.scope().clone(), f.as_of().clone())
    };
    p.ticked = scope.dimensions.iter().find(|d| d.column == column).map(|d| d.values.iter().cloned().collect()).unwrap_or_default();
    let mut minus_own = scope;
    minus_own.dimensions.retain(|d| d.column != column);
    let tag = p.tag;
    cx.emit(ShellEvent::DistinctRequested(DistinctParams { key: PICKER_KEY, tag, column: column.to_string(), scope: minus_own, as_of }));
}
```

`deliver_distinct` on `ShellView`: drop if `self.picker` is `None`, the
stage's column differs, or `outcome.tag != p.tag`; else
`p.values = Some(outcome.values)`; `cx.notify()`.

The `dialog_input` feeds `p.query` through the same `InputEvent::Change`
subscription the keybinding/settings dialogs use (find where
`dialog_input`'s subscription routes to `keybindings`/`settings` state
and add the `picker` arm: set `query`, reset `selected` to 0).

`handle_key` (the `ModalKeyHandler`): on `Stage::Columns` — `enter`
moves to `Values` for the selected column (clear the input value via
`set_value("", ..)`, `request_values`), up/down/ctrl+p/ctrl+n move
selection, everything else `false`. On `Stage::Values` — `tab` toggles,
`ctrl+a` ticks all shown, `ctrl+x` clears, `enter` applies:

```rust
                let new_scope = p.apply(shell.frame.read(cx).scope());
                shell.frame.update(cx, |f, cx| { if f.set_scope(new_scope) { cx.notify(); } });
                shell.close_modal(window, cx);   // the existing close path used by escape
                true
```

up/down/ctrl+p/ctrl+n move selection over `shown().len()`; `escape`
returns `false` so the modal's own escape closes it (which also clears
`view.picker` — add that to the modal close path where `keybindings`
and `settings` are cleared at `mod.rs:~730`).

`build`: `filter_row(&shell.dialog_input, None, cx)` on top; below it,
for `Columns`, a list of rows `column · role · datasets` with the
selected row highlighted (copy the palette's row styling); for
`Values`, `loading…` / the error text / a `uniform_list("picker-values",
shown.len(), …)` of rows `[x] value    count` with the tick as `✓` in
`theme.primary` or a muted `·`, count right-aligned in the mono face.
`uniform_list`'s closure receives a `Range<usize>` and returns the rows
for it; it needs the `shown` vector, which is computed once per render
from state (a `Vec<(usize, Vec<usize>)>` — allocation per render is
acceptable in a modal that only re-renders on a keystroke, same as the
palette).

Chip body click (Task 4 left it inert): in `toolbar`, the chip body's
`on_mouse_down` calls an `on_chip_open(column)` closure that runs
`picker::open(view, Some(column), window, cx)`.

`bridge.rs`: in the `ShellEvent` subscription, `DistinctRequested(p) =>
{ handle.distinct(p.clone()); }`; in the drain, `DataEvent::Distinct(o)
=> shell.update(cx, |s, cx| s.deliver_distinct(o, cx))`.

- [ ] **Step 5: e2e tests**

`tests/picker.rs`:

```rust
#[gpui::test]
fn the_picker_requests_values_minus_its_own_selection_and_applies_ticks_as_one_scope_change(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, services_with_pickable());   // datasets = CARRIED_DEMO; registry has frame::pick_book
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    let requested = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    vcx.update(|_, cx| {
        let requested = requested.clone();
        cx.subscribe(&shell, move |_, e: &ShellEvent, _| {
            if let ShellEvent::DistinctRequested(p) = e { requested.borrow_mut().push(p.clone()); }
        }).detach();
    });
    frame.update(&mut vcx, |f, cx| {
        f.set_scope(Scope { dimensions: vec![
            DimensionSelection { column: "book".into(), values: vec!["BK000".into()] },
            DimensionSelection { column: "lhu".into(), values: vec!["L1".into()] },
        ], ..Scope::default() });
        cx.notify();
    });
    shell.update(&mut vcx, |s, cx| s.dispatch(&ActionId("frame::pick_book".into()), None, /* window */ &mut *cx_window, cx));
    // (use the tests' existing `dispatch_action` helper if one exists; else simulate the palette route)
    let req = requested.borrow().last().cloned().expect("a distinct request");
    assert_eq!(req.column, "book");
    assert_eq!(req.scope.dimensions.len(), 1, "own selection removed");
    assert_eq!(req.scope.dimensions[0].column, "lhu");
    // Deliver values; BK000 is pre-ticked.
    shell.update(&mut vcx, |s, cx| s.deliver_distinct(DistinctOutcome {
        key: PICKER_KEY, tag: req.tag, column: "book".into(),
        values: Ok(vec![("BK000".into(), 5), ("BK001".into(), 7), ("BK002".into(), 1)]),
    }, cx));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("picker-value-BK001").is_some());
    let v0 = frame.read_with(&vcx, |f, _| f.versions().scope);
    vcx.simulate_keystrokes("down tab enter");   // tick BK001, apply
    assert_eq!(frame.read_with(&vcx, |f, _| f.versions().scope), v0 + 1, "one scope change");
    let books = frame.read_with(&vcx, |f, _| f.scope().dimensions.iter().find(|d| d.column == "book").unwrap().values.clone());
    assert_eq!(books, vec!["BK000".to_string(), "BK001".to_string()]);
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_none()));
}

#[gpui::test]
fn a_stale_distinct_outcome_is_dropped(cx: &mut gpui::TestAppContext) {
    // open picker for book (tag 1), deliver tag 0 → still loading
}

#[gpui::test]
fn escape_cancels_without_touching_the_scope(cx: &mut gpui::TestAppContext) { /* … */ }
```

Fill in the two sketches fully. The dispatch route: look at how
`tests/palette.rs` dispatches an action by id through the palette
(`simulate_keystrokes("ctrl-k")`, type the title, `enter`) and use that
if there is no direct helper.

- [ ] **Step 6: Run, harness, commit**

```sh
run_mutation "picker: the request omits the column's own selection" \
  crates/geode-shell/src/shell/picker.rs \
  '    minus_own.dimensions.retain(|d| d.column != column);' \
  '    let _ = &minus_own;' \
  geode-shell the_picker_requests_values_minus_its_own_selection_and_applies_ticks_as_one_scope_change

run_mutation "picker: a stale outcome is dropped" \
  crates/geode-shell/src/shell/mod.rs \
  '<the tag compare line in deliver_distinct>' \
  '<same line with the compare removed>' \
  geode-shell a_stale_distinct_outcome_is_dropped

run_mutation "picker: an empty tick set drops the chip" \
  crates/geode-shell/src/shell/picker.rs \
  '        if !self.ticked.is_empty() {' \
  '        if true {' \
  geode-shell apply_replaces_the_columns_selection_and_an_empty_tick_set_drops_it

run_mutation "pickable: keys are not pickable" \
  crates/geode-shell/src/shell/mod.rs \
  '<the categorical_columns() call in pickable_columns>' \
  '<replaced by every utf8 column>' \
  geode-shell pickable_columns_are_every_categorical_column_plus_derived_dimensions
```

Fill the placeholders with the exact lines once written; an entry with
a wrong anchor is `ANCHOR-MISSING`, which the run reports.

```bash
git add -A
git commit -m "feat(shell): dimension pickers over Request::Distinct, frame::pick and per-column pick actions (Phase 4 §3.3-3.4)"
```

---

### Task 6: The as-of selector and the historical indicator

Spec §3.6, §3.11 (`frame::as_of`, `frame::live`, `frame::as_of_undo`).

**Files:**
- Create: `crates/geode-shell/src/shell/asof_view.rs`
- Modify: `crates/geode-shell/src/shell/mod.rs` (`as_of_dialog: Option<AsOfState>`), `input.rs`, `defaults.rs`, `render.rs` (stripe), `status.rs` (segment)
- Create: `crates/geode-shell/src/shell/tests/asof.rs`
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Consumes: `parse_as_of` (core), `Frame::{recent_publishes, set_as_of, undo_as_of, as_of}`.
- Produces:
  ```rust
  pub struct AsOfState { pub selected: usize, pub error: Option<String>, pub resolved: Option<DateTime<Utc>> }
  pub fn presets(frame: &Frame) -> Vec<(DateTime<Utc>, String)>;   // newest first, label "14:05:12 · risk / EOD · 3 books"
  pub fn open(view: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>);
  // status_bar gains `as_of: Option<&str>`; render paints `as-of-stripe` when historical
  ```

- [ ] **Step 1: Failing tests**

`asof_view.rs` unit tests: `presets` labels and ordering from a frame
with three publishes; `resolve_input("14:05", now)` returns today at
14:05 UTC (delegating to `parse_as_of`); `"live"` resolves to
`AsOf::Live`; garbage returns `Err` with the parser's message.

`tests/asof.rs`:

```rust
#[gpui::test]
fn typing_a_time_and_enter_sets_as_of_and_paints_the_stripe_and_segment(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    assert!(vcx.debug_bounds("as-of-stripe").is_none());
    vcx.simulate_keystrokes("ctrl-t");
    vcx.simulate_input("14:05");
    vcx.simulate_keystrokes("enter");
    assert!(matches!(frame.read_with(&vcx, |f, _| f.as_of().clone()), AsOf::At(_)));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("as-of-stripe").is_some());
    assert!(vcx.debug_bounds("status-as-of").is_some());
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_none()));
}

#[gpui::test]
fn a_bad_time_shows_inline_and_enter_does_nothing(cx: &mut gpui::TestAppContext) { /* "nope" → modal stays, error painted as "as-of-error", frame live */ }

#[gpui::test]
fn selecting_a_preset_sets_that_instant_and_live_then_undo_returns(cx: &mut gpui::TestAppContext) {
    // note_published twice, open, "down enter" picks the second-newest; frame::live; frame::as_of_undo restores
}
```

- [ ] **Step 2: Implement**

`asof_view.rs` follows `keybindings_view` exactly: `open` sets
`view.as_of_dialog = Some(AsOfState::default())`, opens with title
"As of", a `handle_key` that on `enter` reads the `dialog_input` value:
empty and a preset selected → set that instant; `"live"` → `AsOf::Live`;
otherwise `parse_as_of` → `set_as_of(AsOf::At(t))` or `state.error =
Some(msg)` and `true` (modal stays). On `InputEvent::Change` (the same
dialog-input subscription arm pattern as the picker): re-resolve and
store `resolved`/`error` so the modal shows "→ 2026-09-06 14:05:00 UTC"
or the error under the field (`as-of-error` selector). Up/down move
over presets; `escape` falls through.

`presets`:

```rust
pub fn presets(frame: &Frame) -> Vec<(DateTime<Utc>, String)> {
    frame
        .recent_publishes()
        .iter()
        .map(|p| (p.at, format!("{} · {} / {} · {} book{}", p.at.with_timezone(&Local).format("%H:%M:%S"), p.dataset, p.batch, p.books, if p.books == 1 { "" } else { "s" })))
        .collect()
}
```

Actions: `frame::as_of` (`mod+t`), `frame::live`, `frame::as_of_undo`,
registered and dispatched (`live` → `set_as_of(AsOf::Live)`, undo →
`undo_as_of`; both notify on `true`).

`render.rs`: between the toolbar and `body`, when
`self.frame.read(cx).as_of()` is `At`, a
`div().w_full().h(px(3.)).bg(theme.warning).debug_selector(|| "as-of-stripe".into())`.
Subtract 3 px from `content_height` in that case so the layout does not
overflow. `status_bar` gains `as_of: Option<&str>` and paints
`AS OF {t} · :live to return` in the warning tokens with selector
`status-as-of`; `render.rs` passes `model.as_of.as_deref()` from the bar
model it already holds.

- [ ] **Step 3: Run, harness, commit**

```sh
run_mutation "as-of: a bad time never sets the frame" \
  crates/geode-shell/src/shell/asof_view.rs \
  '<the Err arm that stores state.error>' \
  '<same arm also calling set_as_of(AsOf::At(Utc::now()))>' \
  geode-shell a_bad_time_shows_inline_and_enter_does_nothing

run_mutation "as-of: the stripe is painted only when historical" \
  crates/geode-shell/src/shell/render.rs \
  '<the AsOf::At match guarding the stripe>' \
  '<true>' \
  geode-shell typing_a_time_and_enter_sets_as_of_and_paints_the_stripe_and_segment
```

(The second entry needs the test to also assert the stripe is *absent*
before the as-of is set, which the test above does.)

```bash
git add -A
git commit -m "feat(shell): the as-of selector with generation presets, the stripe and the status segment (Phase 4 §3.6)"
```

---

### Task 7: The blotter — `:filter`, and the new `:scope`/`:asof` forms

Spec §3.7, §3.11 (the `:` vocabulary additions).

**Files:**
- Modify: `crates/geode-blotter/src/core/commands.rs`
- Modify: `crates/geode-blotter/src/tile.rs` (`command`, `serialize`, restore, the header)
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Consumes: `Frame::{drop_dimension, redo_scope, save_scope, load_scope, undo_as_of, saved_scopes}`, `Expr: Display`, `parse_expr`.
- Produces: `Command::{FilterExpr(String), FilterText(String), FilterClear, ScopeDrop(String), ScopeRedo, ScopeSave(String), ScopeLoad(String), AsOfUndo}`; `Vocabulary.scopes: Vec<String>`; tile record keys `filter.expr`, `filter.text`; header pill `filtered`.

- [ ] **Step 1: Grammar tests, failing**

In `commands.rs` tests:

```rust
    #[test]
    fn filter_forms_parse() {
        assert_eq!(parse("filter npv > 0").unwrap(), Command::FilterExpr("npv > 0".into()));
        assert_eq!(parse("filter text spx").unwrap(), Command::FilterText("spx".into()));
        assert_eq!(parse("filter clear").unwrap(), Command::FilterClear);
        assert!(parse("filter").unwrap_err().contains("filter"));
    }

    #[test]
    fn new_scope_and_asof_forms_parse() {
        assert_eq!(parse("scope drop book").unwrap(), Command::ScopeDrop("book".into()));
        assert_eq!(parse("scope redo").unwrap(), Command::ScopeRedo);
        assert_eq!(parse("scope save mine").unwrap(), Command::ScopeSave("mine".into()));
        assert_eq!(parse("scope load mine").unwrap(), Command::ScopeLoad("mine".into()));
        assert_eq!(parse("asof undo").unwrap(), Command::AsOfUndo);
        assert!(parse("scope drop").unwrap_err().contains("dimension"));
        assert!(parse("scope save").unwrap_err().contains("name"));
    }

    #[test]
    fn completions_offer_dimensions_after_drop_and_scope_names_after_load() {
        let vocab = Vocabulary { columns: vec!["book".into()], views: vec![], scopes: vec!["mine".into()] };
        assert_eq!(completions("scope drop ", 11, &vocab), vec!["book"]);
        assert_eq!(completions("scope load ", 11, &vocab), vec!["mine"]);
        assert!(completions("", 0, &vocab).contains(&"filter".to_string()));
    }
```

- [ ] **Step 2: Implement the grammar**

Extend `Command`, `COMMANDS` (add `"filter"`), the `"scope"` arm:

```rust
        "scope" => {
            let mut words = rest.splitn(2, char::is_whitespace);
            match (words.next(), words.next().map(str::trim)) {
                (Some("clear"), None) => Ok(Command::ScopeClear),
                (Some("undo"), None) => Ok(Command::ScopeUndo),
                (Some("redo"), None) => Ok(Command::ScopeRedo),
                (Some("drop"), Some(d)) if !d.is_empty() => Ok(Command::ScopeDrop(d.to_string())),
                (Some("drop"), _) => Err("scope drop needs a dimension".into()),
                (Some("save"), Some(n)) if !n.is_empty() => Ok(Command::ScopeSave(n.to_string())),
                (Some("save"), _) => Err("scope save needs a name".into()),
                (Some("load"), Some(n)) if !n.is_empty() => Ok(Command::ScopeLoad(n.to_string())),
                (Some("load"), _) => Err("scope load needs a name".into()),
                (Some("text"), Some(w)) => Ok(Command::ScopeText(w.to_string())),
                (Some("text"), None) => Ok(Command::ScopeText(String::new())),
                (Some(_), _) => Ok(Command::ScopeExpr(rest.to_string())),
                (None, _) => Err("scope needs an expression, `text …`, `clear`, `undo`, `redo`, `drop <dim>`, `save <name>` or `load <name>`".into()),
            }
        }
        "filter" => match rest.split_once(char::is_whitespace) {
            None if rest == "clear" => Ok(Command::FilterClear),
            None if rest.is_empty() => Err("filter needs an expression, `text …` or `clear`".into()),
            Some(("text", words)) => Ok(Command::FilterText(words.trim().to_string())),
            _ => Ok(Command::FilterExpr(rest.to_string())),
        },
        "asof" => match rest {
            "" => Err("asof needs a time: HH:MM, HH:MM:SS or RFC 3339, or `undo`".into()),
            "undo" => Ok(Command::AsOfUndo),
            t => Ok(Command::AsOf(t.to_string())),
        },
```

`Vocabulary` gains `scopes`; `completions` adds `["scope", "drop"] =>
columns`, `["scope", "load"] => scopes`, `["scope"] => the subcommand
words plus columns`, `["filter"] => ["text", "clear"] plus columns`,
`["asof"] => ["undo"]`.

- [ ] **Step 3: Tile tests, failing**

In `tile.rs` tests (the module has a `TestAppContext` harness with a
fake `DataHandle` via `for_tests` — reuse):

```rust
    #[gpui::test]
    fn filter_narrows_only_this_tile_marks_it_and_round_trips_the_session(cx: &mut gpui::TestAppContext) {
        // two tiles over one frame; `:filter model_code = 'EURP'` on tile A:
        // - tile A requeries with scope.expression set; tile B does not requery
        // - A's header paints "blotter-filtered-<id>"
        // - A.serialize() has filter.expr == "model_code = 'EURP'"
        // - a new tile restored from that record has the same tile_scope
        // - `:filter clear` clears and the pill goes
    }

    #[gpui::test]
    fn an_unscoped_tile_still_applies_its_own_filter(cx: &mut gpui::TestAppContext) { /* :unscoped then :filter text x → the query's scope has text x and no frame dimensions */ }

    #[gpui::test]
    fn filter_validates_against_the_tiles_dataset(cx: &mut gpui::TestAppContext) { /* `:filter nope = 1` → Err naming nope; tile_scope unchanged */ }
```

Write them fully against the existing helpers (grep `for_tests` and
`recv_timeout` in `tile.rs` tests for how a submitted `QueryParams` is
read back).

- [ ] **Step 4: Implement in the tile**

`command` arms:

```rust
            Command::FilterExpr(text) => {
                let expr = parse_expr(&text).map_err(|e| format!("{} at column {}", e.message, e.caret + 1))?;
                let mut scope = self.tile_scope.clone();
                scope.expression = Some(expr);
                self.validate_tile_scope(&scope)?;
                self.tile_scope = scope;
                self.requery(cx);
            }
            Command::FilterText(words) => {
                let mut scope = self.tile_scope.clone();
                scope.text = (!words.trim().is_empty()).then_some(words);
                self.tile_scope = scope;
                self.requery(cx);
            }
            Command::FilterClear => {
                self.tile_scope = Scope::default();
                self.requery(cx);
            }
            Command::ScopeDrop(d) => { if !self.frame.update(cx, |f, cx| { let r = f.drop_dimension(&d); if r { cx.notify(); } r }) { return Err(format!("no selection on '{d}'")); } }
            Command::ScopeRedo => { if !self.frame.update(cx, |f, cx| { let r = f.redo_scope(); cx.notify(); r }) { return Err("nothing to redo".into()); } }
            Command::ScopeSave(name) => { self.frame.update(cx, |f, cx| { let r = f.save_scope(&name); cx.notify(); r })?; }
            Command::ScopeLoad(name) => { self.frame.update(cx, |f, cx| { let r = f.load_scope(&name); cx.notify(); r })?; }
            Command::AsOfUndo => { if !self.frame.update(cx, |f, cx| { let r = f.undo_as_of(); cx.notify(); r }) { return Err("no previous as-of".into()); } }
```

`validate_tile_scope`: `scope.validate(&dataset_spec, &dims)` needs the
dataset spec and derived dimensions; the tile has `views` but not the
schema. Add `schema: Rc<RefCell<SchemaSpec>>` and `dims: Rc<RefCell<DerivedDimensions>>`
to `BlotterFactory` (set from the bridge beside `set_views`, refreshed
on `ConfigReloaded`) and thread them into `BlotterTile::new` like
`views`. First error's message is the `Err`.

`serialize`: when `tile_scope` is non-empty, insert
`filter = { expr = "<Display>", text = "<text>" }` (omit absent keys).
Restore in `new`: read `filter.expr` through `parse_expr` (a parse
failure drops the filter with an `eprintln!` warning — 4b migrates it)
and `filter.text`. Header: a `filtered` pill after `unscoped`, selector
`blotter-filtered-{tile}`. `completions` fills `Vocabulary.scopes` from
`frame.saved_scopes().keys()`.

- [ ] **Step 5: Run, harness, commit**

```sh
run_mutation "blotter: :filter narrows only this tile" \
  crates/geode-blotter/src/tile.rs \
  '                self.tile_scope = scope;
                self.requery(cx);' \
  '                self.requery(cx);' \
  geode-blotter filter_narrows_only_this_tile_marks_it_and_round_trips_the_session

run_mutation "blotter: an unscoped tile keeps its own filter" \
  crates/geode-blotter/src/tile.rs \
  '                self.tile_scope.clone()' \
  '                Scope::default()' \
  geode-blotter an_unscoped_tile_still_applies_its_own_filter

run_mutation "blotter: filter validates against the dataset" \
  crates/geode-blotter/src/tile.rs \
  '                self.validate_tile_scope(&scope)?;' \
  '                let _ = self.validate_tile_scope(&scope);' \
  geode-blotter filter_validates_against_the_tiles_dataset

run_mutation "commands: scope drop needs a dimension" \
  crates/geode-blotter/src/core/commands.rs \
  '                (Some("drop"), _) => Err("scope drop needs a dimension".into()),' \
  '                (Some("drop"), _) => Ok(Command::ScopeDrop(String::new())),' \
  geode-blotter new_scope_and_asof_forms_parse
```

```bash
git add -A
git commit -m "feat(blotter): :filter as the tile layer, :scope drop/redo/save/load, :asof undo (Phase 4 §3.7, §3.11)"
```

---

### Task 8: The flip barrier

Spec §3.10. Every following tile swaps to a new scope, grouping or
as-of in one notify pass, with a 250 ms deadline.

**Files:**
- Modify: `crates/geode-shell/src/frame.rs` (`FlipBarrier`, `versions.flip`, `open_flip`, `arrived`, `sweep`, `flip`, `barrier_for`)
- Modify: `crates/geode-shell/src/shell/mod.rs` (`on_frame_changed` opens the barrier; the deadline timer; `last_flip_versions`)
- Modify: `crates/geode-shell/src/shell/occupants.rs` (`visible_tile_keys`)
- Modify: `crates/geode-blotter/src/tile.rs` (`staged`, `deliver`, `on_frame_changed`)
- Create: `crates/geode-shell/src/shell/tests/flip.rs`
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Produces:
  ```rust
  pub const FLIP_DEADLINE: Duration = Duration::from_millis(250);
  pub struct FrameVersions { /* … */ pub flip: u64 }
  impl Frame {
      /// Open a barrier for the current (scope, grouping, as_of) versions over `keys`.
      pub fn open_flip(&mut self, keys: impl IntoIterator<Item = QueryKey>, now: Instant);
      /// Whether an open barrier is waiting for `key` at `versions`.
      pub fn barrier_wants(&self, key: QueryKey, versions: FrameVersions) -> bool;
      /// `true` when this arrival emptied the barrier (and `flip` bumped).
      pub fn arrived(&mut self, key: QueryKey, versions: FrameVersions) -> bool;
      /// Past the deadline, release with whatever arrived. `true` when released.
      pub fn sweep(&mut self, now: Instant) -> bool;
      pub fn barrier_open(&self) -> bool;
  }
  // ShellView
  fn visible_tile_keys(&self, out: &mut Vec<QueryKey>);
  ```

- [ ] **Step 1: Pure barrier tests, failing**

```rust
    #[test]
    fn a_barrier_releases_when_every_key_arrives_and_bumps_flip_once() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.set_scope(book_scope("A"));
        let v = f.versions();
        let t0 = Instant::now();
        f.open_flip([QueryKey(1), QueryKey(2)], t0);
        assert!(f.barrier_open());
        assert!(f.barrier_wants(QueryKey(1), v));
        assert!(!f.barrier_wants(QueryKey(3), v));
        let mut stale = v; stale.scope -= 1;
        assert!(!f.barrier_wants(QueryKey(1), stale));
        assert!(!f.arrived(QueryKey(1), v));
        assert_eq!(f.versions().flip, v.flip);
        assert!(f.arrived(QueryKey(2), v));
        assert_eq!(f.versions().flip, v.flip + 1);
        assert!(!f.barrier_open());
        assert!(!f.arrived(QueryKey(2), v), "nothing open");
    }

    #[test]
    fn the_deadline_releases_with_whatever_arrived() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        let v = f.versions();
        let t0 = Instant::now();
        f.open_flip([QueryKey(1), QueryKey(2)], t0);
        assert!(!f.sweep(t0 + Duration::from_millis(100)));
        assert!(f.sweep(t0 + FLIP_DEADLINE + Duration::from_millis(1)));
        assert_eq!(f.versions().flip, v.flip + 1);
        assert!(!f.barrier_open());
    }

    #[test]
    fn a_new_mutation_while_open_replaces_the_barrier() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.open_flip([QueryKey(1)], Instant::now());
        let v_old = f.versions();
        f.set_scope(book_scope("B"));
        let v_new = f.versions();
        f.open_flip([QueryKey(1), QueryKey(2)], Instant::now());
        assert!(!f.barrier_wants(QueryKey(1), v_old));
        assert!(f.barrier_wants(QueryKey(2), v_new));
    }

    #[test]
    fn data_and_config_bumps_do_not_open_a_barrier_and_do_not_match_one() {
        // versions with only `data` different must not satisfy barrier_wants
    }
```

- [ ] **Step 2: Implement in `Frame`**

```rust
struct FlipBarrier { scope: u64, grouping: u64, as_of: u64, awaiting: HashSet<QueryKey>, opened: Instant }

    pub fn open_flip(&mut self, keys: impl IntoIterator<Item = QueryKey>, now: Instant) {
        let awaiting: HashSet<QueryKey> = keys.into_iter().collect();
        if awaiting.is_empty() {
            self.barrier = None;
            return;
        }
        self.barrier = Some(FlipBarrier { scope: self.versions.scope, grouping: self.versions.grouping, as_of: self.versions.as_of, awaiting, opened: now });
    }

    fn matches(b: &FlipBarrier, v: FrameVersions) -> bool {
        b.scope == v.scope && b.grouping == v.grouping && b.as_of == v.as_of
    }

    pub fn barrier_wants(&self, key: QueryKey, versions: FrameVersions) -> bool {
        self.barrier.as_ref().is_some_and(|b| Self::matches(b, versions) && b.awaiting.contains(&key))
    }

    pub fn arrived(&mut self, key: QueryKey, versions: FrameVersions) -> bool {
        let Some(b) = self.barrier.as_mut() else { return false; };
        if !Self::matches(b, versions) { return false; }
        b.awaiting.remove(&key);
        if b.awaiting.is_empty() { self.release(); true } else { false }
    }

    pub fn sweep(&mut self, now: Instant) -> bool {
        match &self.barrier {
            Some(b) if now.duration_since(b.opened) >= FLIP_DEADLINE => { self.release(); true }
            _ => false,
        }
    }

    fn release(&mut self) {
        self.barrier = None;
        self.versions.flip += 1;
    }
```

`FrameVersions` gains `flip`; **`follows_changed` in the blotter must
not compare it** (it lists the five counters explicitly, so nothing
changes there — add a comment saying `flip` is deliberately excluded).
`bar_model`'s cache key includes `flip` via `versions()`; that is one
rebuild per flip, harmless, but exclude it anyway by keying the cache
on a copy with `flip: 0`.

- [ ] **Step 3: Shell opens the barrier and runs the deadline**

`ShellView.last_flip_versions: FrameVersions`. In `on_frame_changed`:

```rust
        let now_v = frame.read(cx).versions();
        let last = self.last_flip_versions;
        if now_v.scope != last.scope || now_v.grouping != last.grouping || now_v.as_of != last.as_of {
            self.last_flip_versions = now_v;
            let mut keys = Vec::new();
            self.visible_tile_keys(&mut keys);
            frame.update(cx, |f, _| f.open_flip(keys.iter().copied(), Instant::now()));
            let frame = frame.clone();
            cx.spawn(async move |_this, cx| {
                cx.background_executor().timer(FLIP_DEADLINE).await;
                let _ = frame.update(cx, |f, cx| { if f.sweep(Instant::now()) { cx.notify(); } });
            })
            .detach();
        }
```

`open_flip` bumps no version, so the notify it does not emit cannot
re-enter this branch. `visible_tile_keys`: `fill_active_tiles` mapped to
`QueryKey(tile.0)`, only tiles with an occupant.

- [ ] **Step 4: The blotter stages**

`BlotterTile.staged: Option<(Arc<Snapshot>, Vec<String>)>` and
`last_flip: u64`. In `deliver`, after the tag check and timing:

```rust
        let acted = self.acted.unwrap_or_default();
        match outcome.snapshot {
            Ok(snapshot) => {
                self.error = None;
                let wants = self.frame.read(cx).barrier_wants(QueryKey(self.tile.0), acted);
                if wants {
                    self.staged = Some((snapshot, self.last_grouping.clone()));
                    let released = self.frame.update(cx, |f, cx| { let r = f.arrived(QueryKey(self.tile.0), acted); if r { cx.notify(); } r });
                    if released { self.promote(cx); }
                } else {
                    self.apply(snapshot, self.last_grouping.clone(), cx);
                }
            }
            Err(e) => {
                self.error = Some(e);
                // Failure counts as arrival: one broken tile never holds the rest.
                self.frame.update(cx, |f, cx| { if f.arrived(QueryKey(self.tile.0), acted) { cx.notify(); } });
            }
        }
```

`apply` is today's `Ok` body (apply_snapshot, refresh, cursor,
`delivered_at`). `promote` takes `staged` and calls `apply`. In
`on_frame_changed`, before the `follows_changed` check:

```rust
        let flip = self.frame.read(cx).versions().flip;
        if flip != self.last_flip {
            self.last_flip = flip;
            self.promote(cx);
        }
```

A tile that does not follow the changed field (pinned under a grouping
change, unscoped under a scope change) still sits in the barrier set;
in `on_frame_changed`, when `!self.follows_changed(now)` and the frame
`barrier_wants(my key, now)`, call `arrived` so it does not hold the
others. A hidden tile is not in the set (`fill_active_tiles`).

- [ ] **Step 5: e2e test**

`tests/flip.rs` with the recording module is not enough (it has no
snapshot); use the blotter's own test harness in `geode-blotter`
instead — add there:

```rust
    #[gpui::test]
    fn two_tiles_promote_in_the_same_pass_and_a_failure_releases_the_barrier(cx: &mut gpui::TestAppContext) {
        // frame shared by tiles 1 and 2 (DataHandle::for_tests); shell-less: open the barrier by hand
        // set_scope → both requery (two QueryParams on rx)
        // frame.open_flip([1,2])
        // deliver tile 1's Ok outcome → tile 1 shows no new snapshot yet (rows unchanged), barrier open
        // deliver tile 2's Ok outcome → both tiles' rows are the new snapshot after one run_until_parked
        // repeat with tile 2 delivering Err → barrier released, tile 1 promoted, tile 2 keeps last-good + error
    }
```

and in `geode-shell/tests/flip.rs`, with the recorder: a scope change
opens a barrier over exactly the visible tiles (`barrier_open()` true;
`barrier_wants` for the visible tile's key, false for a hidden one) and
`sweep` at the deadline releases it.

- [ ] **Step 6: Run, harness, commit**

```sh
run_mutation "flip: failure counts as arrival" \
  crates/geode-blotter/src/tile.rs \
  '                self.frame.update(cx, |f, cx| { if f.arrived(QueryKey(self.tile.0), acted) { cx.notify(); } });' \
  '                let _ = acted;' \
  geode-blotter two_tiles_promote_in_the_same_pass_and_a_failure_releases_the_barrier

run_mutation "flip: a staged snapshot waits for the barrier" \
  crates/geode-blotter/src/tile.rs \
  '                if wants {' \
  '                if false {' \
  geode-blotter two_tiles_promote_in_the_same_pass_and_a_failure_releases_the_barrier

run_mutation "flip: the deadline releases" \
  crates/geode-shell/src/frame.rs \
  '            Some(b) if now.duration_since(b.opened) >= FLIP_DEADLINE => { self.release(); true }' \
  '            Some(_) if false => { self.release(); true }' \
  geode-shell the_deadline_releases_with_whatever_arrived

run_mutation "flip: only scope/grouping/as-of open a barrier" \
  crates/geode-shell/src/shell/mod.rs \
  '        if now_v.scope != last.scope || now_v.grouping != last.grouping || now_v.as_of != last.as_of {' \
  '        if now_v != last {' \
  geode-shell <the tests/flip.rs test asserting a data bump opens nothing>
```

```bash
git add -A
git commit -m "feat(frame): the flip barrier — every following tile swaps in one pass, 250 ms deadline (Phase 4 §3.10)"
```

---

### Task 9: Docs, the spec amendments, the full harness run

**Files:**
- Modify: `CLAUDE.md` (a **Phase 4a is done** paragraph after the Phase 3c ones; the "Workspace invariants" list gains: *`FrameVersions.flip` is excluded from `follows_changed`; a tile that compares it requeries on every flip*)
- Modify: `docs/perf.md` (the Phase 4a section from Task 2, plus the painted-frame reading for a per-keystroke text filter in `--demo` at 1M rows: type five characters with the perf overlay on and record the requery p50/p95)
- Modify: `docs/superpowers/specs/2026-09-06-geode-phase-4-frame-features-design.md` §3.3: `tab` toggles, `ctrl+a` ticks all shown, `ctrl+x` clears (Task 5's amendment), and §3.11's table if any default chord moved
- Modify: `scripts/mutation-check.sh` header count ("180 entries" in `CLAUDE.md` → the new total)

- [ ] **Step 1: Docs**

Write the `CLAUDE.md` paragraph in the voice of the Phase 3 ones: what
landed (carried dimensions and `categorical`, the ENUM text rewrite with
its numbers, the scope bar and its keys, pickers, the as-of selector,
`:filter`, undo/redo, saved scopes, the flip barrier), the demo-database
deletion note, and the two things a maintainer must know: `flip` is
excluded from `follows_changed`, and a `dimension` with no `grain`
outside the built-in keys is dropped at load.

- [ ] **Step 2: The unfiltered harness run**

```bash
git status --short   # must be clean
nohup zsh scripts/mutation-check.sh > /tmp/mutation-4a.log 2>&1 &
```

Watch with `pgrep -f "^zsh scripts/mutation-check.sh"`; when it ends,
`grep -E "SURVIVED|ANCHOR-MISSING" /tmp/mutation-4a.log` must be empty.
A `SURVIVED` is a missing test, fixed before merge.

- [ ] **Step 3: Done-state walk in `--demo`**

Run `cargo run -p geode-app -- --demo 1000000` and walk spec §1.2's 4a
items: type in the bar; `mod+p` `book`; `mod+p` `currency`; `:group
currency,underlying_ref`; `mod+t` `14:05`, `:live`, `:asof undo`;
`:filter model_code = 'EURP'`; `:scope save mine` then the palette's
`Scope: mine`; two tiles flipping together on a picker apply. Record
the requery readings in `docs/perf.md`.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "docs: Phase 4a — CLAUDE.md, perf.md text-filter numbers, spec §3.3 picker keys"
```

Then the branch's final review and merge per the working rhythm.

---

## Self-Review

**Spec coverage (4a items, §1.1):**
- Scope bar, chips, live field, cached model — Tasks 3, 4.
- Carried dimensions, `categorical` — Task 1.
- Pickers, `frame::pick`, per-column actions, `Request::Distinct` — Tasks 2, 5.
- Per-keystroke text filter, ENUM rewrite, bench gate — Tasks 2, 4.
- `textual` load-time validation — Task 1.
- As-of selector, presets, stripe, segment, `:asof undo` — Tasks 3, 6, 7.
- `:filter` — Task 7.
- Undo/redo stacks — Task 3.
- Saved scopes — Tasks 3 (reader, frame API, palette items), 7 (`:scope save/load`).
- Flip barrier — Task 8.
- Session `[frame]` — Task 3.
- `mod+/`, `mod+p`, `mod+t` — Tasks 4, 5, 6; `mod+z`/`mod+shift+z` and `frame::scope_clear` — Task 3 Step 6.

**Placeholder scan:** Task 5 Step 6 and Task 6 Step 3 leave harness
anchors as `<…>` because the exact line does not exist until the code
is written; each says so and names what the anchor must hit. Task 5
Step 5's second and third tests are sketches with their assertions
stated in comments and an instruction to write them fully. Task 8
Step 5's blotter test likewise. These are deliberate: the assertions
are specified, only the harness-specific plumbing is left to the
implementer, who has the neighbouring tests as the model.

**Type consistency:** `Frame::new(slots, saved, user_dir)` (Task 3) is
used by Tasks 5, 6, 8's tests; `Publish` is what `bridge.rs` builds and
`asof_view::presets` reads; `DistinctParams`/`DistinctOutcome` are
core types used by Tasks 2 and 5 with the same fields; `PICKER_KEY` is
defined once in `shell/mod.rs`; `RequestKind` is data-internal;
`FrameVersions.flip` is added in Task 8 and referenced nowhere earlier.

## Execution Handoff

Plan complete and saved to
`docs/superpowers/plans/2026-09-06-phase-4a-frame-features.md`. Two
execution options:

1. **Subagent-Driven (recommended)** — a fresh subagent per task,
   review between tasks, the ledger under
   `.superpowers/sdd/2026-09-06-phase-4a-frame-features/progress.md`,
   as the 3b/3c sessions ran.
2. **Inline Execution** — execute tasks in this session with
   executing-plans, batch execution with checkpoints.
