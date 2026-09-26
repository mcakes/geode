# View Strictness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a view that cannot be honoured refuse at load with a diagnostic
naming what is wrong, instead of opening and painting the columns it cannot
supply as blank forever.

**Architecture:** `ViewSpec::validate` gains the two checks it documents itself
as lacking, and `DataService` gains the refusal those diagnostics have never
had. An unhonourable join or column can opt out with `required = false`, reusing
`ColumnSpec::required`'s defaults-true vocabulary. The compiler's two
configuration-error `continue`s then become hard errors, because validation has
become the gate.

**Tech Stack:** Rust 2024, DuckDB, `toml` with `preserve_order`.

**Spec:** `docs/superpowers/specs/2026-09-26-geode-silent-wrong-data-design.md`
(§5; §1 and §2 bind the whole spec)

## Global Constraints

- The principle: **identity over position, and refuse rather than guess.** A
  plausible wrong total or a silently blank column is worse than an explicit
  refusal.
- **A diagnostic is the entire remedy the trader gets**, so it names the join or
  column and the reason. A refusal whose message does not say which column is a
  worse outcome than the blank column it replaces.
- Strictness stands by the user's ruling of 2026-09-26. A view that works today
  by being silently wrong refuses tomorrow; that is the intent, not a
  regression.
- `required = false` is a per-declaration opt-out, never a global policy switch
  and never a per-kind heuristic.
- `geode-core` stays free of I/O. Validation is pure; only `geode-data` owns the
  service and the compiler.
- Every views document this repo ships must validate clean, proved by a test.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`, and `zsh scripts/mutation-check.sh --anchors-only`
  (exit 0) all pass at the end of every task.
- No new dependency.

---

## What is actually wrong on current main (38dd7d39)

Verified before this plan was written. **Three of these differ from the spec and
are the reason the task order below is what it is.**

1. **`ViewSpec::validate` already checks that a join's dataset exists**
   (`crates/geode-core/src/view.rs:362-366`), which spec §5.2 lists as new work.
   Only two of its three checks are new.
2. **There is no refusal mechanism at all, so the checks would change nothing.**
   `validate` runs at service open (`crates/geode-data/src/service.rs:1300`) and
   on reload (`:1356`), but its `Severity::Error` diagnostics are only
   *collected*. `DataService::query` (`:1381-1391`) finds the view by name and
   compiles it with no severity check. Only the ad-hoc *regrouped* path
   (`:1403-1414`) refuses. So adding checks without building refusal produces
   diagnostics nobody acts on and the blank column still paints.
3. **`service.rs:1295-1296` claims validation will "Report and skip broken views
   while serving valid ones." Nothing skips.** The comment is false today.
4. A fourth silent-absence case the spec did not name: a `Dimension`-declared
   column that is neither in `view.grouping` nor a column of a joined dataset is
   selected nowhere — the spine selects only materialized grouping columns,
   aggregates, joins and derived (`compile.rs:362-408`, `:713-722`) — so it
   paints blank.
5. As specified: the compiler's three `continue`s (`compile.rs:693-709`, the
   middle one correct and staying), the `filter_map` that drops a
   measure-declared column the primary dataset does not have (`:424-430`), and
   the `_ => Aggregate::Sum` fallback (`:437-439`) that sums a grain-bearing
   attribute into a plausible wrong total.
6. `kind` defaults to `"measure"` when absent (`view.rs:537`), so the most
   probable authoring mistake takes the silent path. 44 of the demo config's
   106 view columns rely on that default.

**The widened blast radius, which follows from finding 2 and which the user's
ruling covers.** Building refusal also enforces the three Error diagnostics
`validate` *already* produces — unknown dataset, unknown column, and a grouping
that shadows a real column. So a view naming an unknown column stops opening,
not merely one with a mis-declared `kind`. This is the same accepted risk §5.3
names, but larger than its wording suggests. Task 1 therefore lands refusal and
the clean-config proof together, before any new check exists.

---

## File Structure

**Task 1 — the refusal, and the proof that shipped configs survive it**
- Modify `crates/geode-data/src/service.rs` — one `validate_views` helper used by
  both open and reload; a `refused_views` map; the check in `query`; the false
  comment.
- Test: `crates/geode-data/src/service.rs` tests, plus a shipped-config test.

**Task 2 — `required` on the declarations**
- Modify `crates/geode-core/src/view.rs` — `JoinSpec.required`, `required` on all
  three `ViewColumn` variants, three constructors, the reader.
- Modify the 35 `ViewColumn` and 3 `JoinSpec` construction sites across
  `geode-core`, `geode-data`, `geode-shell`, `geode-blotter` and one bench.

**Task 3 — the two new checks**
- Modify `crates/geode-core/src/view.rs` — `validate` gains join-key carriage and
  column reachability/role, honouring `required = false`.

**Task 4 — the compiler stops guessing**
- Modify `crates/geode-data/src/query/compile.rs` — two `continue`s become
  `compile_error`; the `Sum` fallback restructured away.

**Task 5 — harness and docs**
- Modify `scripts/mutation-check.sh`, `docs/current/data-path.md`,
  `docs/current/configuration.md`, `crates/geode-core/README.md`,
  `crates/geode-data/README.md`.

---

### Task 1: A view that cannot be honoured is refused

**Files:**
- Modify: `crates/geode-data/src/service.rs` — `:1294-1301` (open), `:1354-1361`
  (reload), `:1381-1391` (`query`), and the `DataService` struct
- Test: `crates/geode-data/src/service.rs` tests module

**Interfaces:**
- Produces: `DataService::refused_views` (private) and a private
  `fn validate_views(&[ViewSpec], &SchemaSpec, &DerivedDimensions) -> (Vec<Diagnostic>, BTreeMap<String, String>)`
- Consumes: nothing from a later task. Locate every line by `grep -n`; the line
  numbers above are from 38dd7d39 and will drift as you edit.

**Why this is first.** Without it, Tasks 3 and 4 produce diagnostics nobody acts
on. With it, the three Error diagnostics `validate` already emits start refusing
— which is the intent, and is why the shipped-config proof belongs here too.

- [ ] **Step 1: Write the failing refusal test**

Add to `crates/geode-data/src/service.rs`'s tests module, directly after
`a_misconfigured_view_is_a_diagnostic_at_open_not_a_binder_error_later`
(`grep -n` for it). That test is the exact fixture you need and shows what the
behaviour is today: the service opens, the diagnostic names the view, and the
query is never refused. Yours adds the refusal and proves a healthy sibling view
still serves:

```rust
    #[test]
    fn a_view_with_an_error_diagnostic_is_refused_by_name_not_compiled() {
        // A diagnostic nobody has a panel open for is not a remedy. Before the
        // refusal, this query compiled: `nosuchcolumn` came back absent, and an
        // absent column paints blank with nothing on screen to say why.
        let (db, _src, _svc, _rx) = service();
        let ds = crate::ingest::load::tests_support::fixture().3;
        let mut schema = SchemaSpec::default();
        schema.datasets.push(ds);

        let good = crate::ingest::load::tests_support::tree_view();
        let mut broken = good.clone();
        broken.name = "broken".into();
        broken.grouping.push("nosuchcolumn".into());

        let (svc, _rx) = DataService::open_channel(DataServiceConfig {
            db_path: db.path().join("geode.duckdb"),
            schema,
            views: vec![good, broken],
            dimensions: DerivedDimensions::default(),
            query_workers: 1,
            sources: Vec::new(),
            adapters: Default::default(),
            documents: Default::default(),
            egress: Vec::new(),
            pricer: PricerConfig::default(),
        })
        .expect("a broken view must not stop the service opening");

        let err = svc
            .query(&params(1, "broken", &Scope::default(), AsOf::Live, 1))
            .expect_err("a view that cannot be honoured must not compile");
        let message = format!("{err}");
        assert!(
            message.contains("nosuchcolumn"),
            "the refusal must name the column — it is the whole remedy: {message}"
        );

        // One broken view must not take the service down with it.
        assert!(
            svc.query(&params(2, "tree", &Scope::default(), AsOf::Live, 1))
                .is_ok(),
            "a healthy view over the same schema still serves"
        );

        // And a grouping override is not a way past the refusal.
        let mut regrouped = params(3, "broken", &Scope::default(), AsOf::Live, 1);
        regrouped.grouping = Some(vec!["book".into()]);
        assert!(svc.query(&regrouped).is_err(), "an override cannot bypass it");
    }
```

Match the `service()` fixture's return arity and the `params(...)` helper's
signature as the neighbouring tests use them; both already exist in that module.

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test -p geode-data a_view_with_an_error_diagnostic_is_refused`
Expected: FAIL — the query succeeds, because nothing refuses yet.

- [ ] **Step 3: One validator for both entry points**

Add near the other free functions in `service.rs`:

```rust
/// Validate every view once, returning the diagnostics to publish and the
/// refusals to enforce.
///
/// One function because `open` and a reload must never disagree about which
/// views are honourable: a view refused at open and served after a reload would
/// be a blotter that works until the next config write.
fn validate_views(
    views: &[ViewSpec],
    schema: &SchemaSpec,
    dimensions: &DerivedDimensions,
) -> (Vec<Diagnostic>, std::collections::BTreeMap<String, String>) {
    let mut diagnostics = Vec::new();
    let mut refused = std::collections::BTreeMap::new();
    for view in views {
        let diags = view.validate(schema, dimensions);
        // The FIRST error is the message the trader sees. Later ones are in
        // the diagnostics panel; repeating them all in a query refusal would
        // bury the one that has to be read.
        if let Some(first) = diags.iter().find(|d| d.severity == Severity::Error) {
            refused.insert(view.name.clone(), first.message.clone());
        }
        diagnostics.extend(diags);
    }
    (diagnostics, refused)
}
```

- [ ] **Step 4: Carry the refusals on the service**

Add to the `DataService` struct, beside `diagnostics`:

```rust
    /// Views whose configuration cannot be honoured, by name, each with the
    /// first error explaining why. A query for one is refused instead of
    /// compiled: the compiler would return the columns it cannot supply as
    /// absent, and an absent column paints blank with nothing on screen to say
    /// why.
    refused_views: std::collections::BTreeMap<String, String>,
```

Replace the open site (`:1294-1301`) — and correct the comment, which claims a
skip that has never happened:

```rust
        // Validate views at open so diagnostics name the configuration before any
        // tile queries it. A view with an error is refused by name in `query`;
        // every other view serves normally.
        let (diagnostics, refused_views) =
            validate_views(&config.views, &config.schema, &config.dimensions);
```

and add `refused_views,` to the struct literal. Replace the reload site
(`:1354-1361`) with the same call, assigning both `self.diagnostics` and
`self.refused_views`, and keep returning the diagnostics it returns today.

- [ ] **Step 5: Refuse in `query`**

In `DataService::query`, immediately after the `unknown view` lookup and before
the grouping-override block:

```rust
        // Refused before the grouping override is considered: a regrouping of a
        // view that cannot be honoured is not a way in.
        if let Some(why) = self.refused_views.get(view) {
            return Err(StoreError::Sql {
                statement: format!("query view '{view}'"),
                source: duckdb::Error::InvalidParameterName(why.clone()),
            });
        }
```

Order matters: after the lookup so an unknown view keeps its own message, before
the regroup so an override cannot bypass the refusal.

- [ ] **Step 6: Run the test**

Run: `cargo test -p geode-data a_view_with_an_error_diagnostic_is_refused`
Expected: PASS.

- [ ] **Step 7: Prove every shipped views document validates clean**

This is spec §5.3's first obligation and the thing that decides whether the
branch is safe to merge. Add to `crates/geode-core/src/view.rs`'s tests module:

```rust
    /// Spec obligation: strictness is only acceptable if what we ship is
    /// already clean. Every views document in the repo is loaded against its
    /// own datasets document and must produce no error diagnostic — otherwise
    /// `--demo` would not open.
    #[test]
    fn every_shipped_views_document_validates_clean() {
        let views_text = include_str!("../../../examples/demo-config/views.toml");
        let datasets_text = include_str!("../../../examples/demo-config/datasets.toml");
        let dims_text = include_str!("../../../examples/demo-config/dimensions.toml");

        let (views, read_diags) = ViewSpec::from_doc(&merge_docs(
            "views",
            &[LayerDoc::builtin("views", views_text).unwrap()],
        ));
        let errors: Vec<&Diagnostic> = read_diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect();
        assert!(errors.is_empty(), "reading the shipped views: {errors:?}");

        let (schema, _) = SchemaSpec::from_doc(&merge_docs(
            "datasets",
            &[LayerDoc::builtin("datasets", datasets_text).unwrap()],
        ));
        let (dims, _) = DerivedDimensions::from_doc(&merge_docs(
            "dimensions",
            &[LayerDoc::builtin("dimensions", dims_text).unwrap()],
        ));

        assert!(!views.is_empty(), "the fixture must actually load views");
        for view in &views {
            let errors: Vec<String> = view
                .validate(&schema, &dims)
                .into_iter()
                .filter(|d| d.severity == Severity::Error)
                .map(|d| d.message)
                .collect();
            assert!(
                errors.is_empty(),
                "shipped view '{}' does not validate: {errors:?}",
                view.name
            );
        }
    }
```

Match the `merge_docs` / `LayerDoc::builtin` spelling the neighbouring tests in
that module already use (`view.rs:1113-1120` shows it). If a shipped view
genuinely fails, STOP and report it — that is a real finding about the shipped
configuration, not a test to loosen.

- [ ] **Step 8: Run it, then the crate, then the workspace**

```bash
cargo test -p geode-core every_shipped_views_document_validates_clean
cargo test -p geode-data
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
Expected: PASS. This test must keep passing after Tasks 3 and 4; it is the
branch's safety net, so if a later task breaks it the config or the check is
wrong, never the test.

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "feat(data): a view that cannot be honoured is refused, not compiled

validate has always reported unknown datasets, unknown columns and shadowed
groupings as errors, and query compiled the view anyway — returning the
columns it could not supply as absent, which paints blank forever. The
comment claiming broken views were skipped described something that never
happened."
```

---

### Task 2: `required` on a join and a column

**Files:**
- Modify: `crates/geode-core/src/view.rs` — `JoinSpec` (`:13-18`), `ViewColumn`
  (`:20-43`), the reader's join block (`:503-523`) and column block (`:525-560`)
- Modify: the construction sites — `crates/geode-data/src/query/compile.rs` (18),
  `crates/geode-core/src/view.rs` (11),
  `crates/geode-shell/src/shell/objectdialog/views.rs` (3),
  `crates/geode-blotter/src/core/plan.rs` (2),
  `crates/geode-blotter/benches/blotter.rs` (1), and 3 `JoinSpec` sites
- Test: `crates/geode-core/src/view.rs` tests module

**Interfaces:**
- Produces:
  - `JoinSpec { dataset: String, on: Vec<String>, required: bool }`
  - `ViewColumn::{Dimension, Measure, Derived}` each with `required: bool`
  - `ViewColumn::measure(name) -> ViewColumn`, `ViewColumn::dimension(name)`,
    `ViewColumn::derived(name, sql)` — all defaulting `required: true`
  - `ViewColumn::required(&self) -> bool`
- Consumes: nothing. Task 3 consumes all of it.

**Why constructors.** 35 construction sites, nearly all in tests, would otherwise
each grow a `required: true` line. The constructors keep the type honest — the
field is real, not a side map — while making the churn one mechanical pass. Do
NOT add a side map keyed by column name: `required` belongs on the declaration
it describes, which is the spec's own reasoning for putting it there rather than
in a global policy.

- [ ] **Step 1: Write the failing reader test**

```rust
    #[test]
    fn required_defaults_true_and_is_read_from_a_join_and_a_column() {
        let text = r#"
[risk]
dataset = "risk_snapshot"
grouping = ["book"]

[[risk.joins]]
dataset = "instrument_ref"
on = ["instrument_ref"]

[[risk.joins]]
dataset = "optional_ref"
on = ["instrument_ref"]
required = false

[[risk.columns]]
name = "delta01"

[[risk.columns]]
name = "maybe_missing"
required = false
"#;
        let (views, _) = ViewSpec::from_doc(&merge_docs(
            "views",
            &[LayerDoc::builtin("views", text).unwrap()],
        ));
        let v = views.iter().find(|v| v.name == "risk").expect("risk");
        assert!(v.joins[0].required, "a join defaults to required");
        assert!(!v.joins[1].required, "required = false is read");
        assert!(v.columns[0].required(), "a column defaults to required");
        assert!(!v.columns[1].required(), "required = false is read");
    }
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test -p geode-core required_defaults_true_and_is_read`
Expected: FAIL — no `required` field.

- [ ] **Step 3: Add the field and the constructors**

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinSpec {
    pub dataset: String,
    /// Join key columns declared by the view.
    pub on: Vec<String>,
    /// Whether the view needs this join to mean what it says. A required join
    /// that cannot be honoured refuses the view; an optional one is dropped
    /// with an informational diagnostic naming it. Defaults to true, the same
    /// way `ColumnSpec::required` does, so silence means "I meant this".
    pub required: bool,
}
```

Give each `ViewColumn` variant a `required: bool` with a doc comment on the enum
stating the same rule once, then:

```rust
impl ViewColumn {
    /// A required measure. `required` is a field rather than a defaulted
    /// builder step because a declaration that omits it means required.
    pub fn measure(name: impl Into<String>) -> ViewColumn {
        ViewColumn::Measure { name: name.into(), required: true }
    }

    pub fn dimension(name: impl Into<String>) -> ViewColumn {
        ViewColumn::Dimension { name: name.into(), required: true }
    }

    pub fn derived(name: impl Into<String>, sql: impl Into<String>) -> ViewColumn {
        ViewColumn::Derived { name: name.into(), sql: sql.into(), required: true }
    }

    pub fn required(&self) -> bool {
        match self {
            ViewColumn::Dimension { required, .. }
            | ViewColumn::Measure { required, .. }
            | ViewColumn::Derived { required, .. } => *required,
        }
    }
}
```

`ViewColumn::name()` keeps working; add `..` to its patterns.

- [ ] **Step 4: Read it in the reader**

In the join block, `required: j.get("required").and_then(|v| v.as_bool()).unwrap_or(true),`.
In the column block, read the same into a local before the `match kind` and pass
it into each variant. A non-bool `required` should produce a diagnostic through
the block's existing `bad(...)` helper rather than being silently ignored —
match how the neighbouring fields report a wrong type; if they ignore it, ignore
it and say so in your report.

- [ ] **Step 5: Convert the construction sites**

Let the compiler drive it. Constructions become the constructors; patterns gain
`..`:

```bash
cargo build --workspace --all-targets 2>&1 | grep -E "^error" | head -40
```

For each construction site, prefer `ViewColumn::measure("x")` over a literal.
Where a test deliberately wants an optional column, write the literal with
`required: false`. Do not add `required: true` to a construction you can replace
with a constructor.

- [ ] **Step 6: Gates and commit**

```bash
cargo test -p geode-core required_defaults_true_and_is_read
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
git add -A
git commit -m "feat(core): a view join and column declare whether they are required

Defaults true, the same vocabulary ColumnSpec::required already uses, so an
existing document means exactly what it meant before."
```

---

### Task 3: The two checks `validate` documents itself as lacking

**Files:**
- Modify: `crates/geode-core/src/view.rs` — the doc comment at `:343-346`, the
  join loop at `:361-366`, and the column loop at `:369-396`
- Test: `crates/geode-core/src/view.rs` tests module

**Interfaces:**
- Consumes: Task 2's `JoinSpec.required` and `ViewColumn::required()`.
- Produces: no new signature. `validate` keeps
  `(&self, &SchemaSpec, &DerivedDimensions) -> Vec<Diagnostic>`.

**The rules, stated precisely.** The spec says "each column's declared kind
matches its actual role", which is right for a measure and wrong for a
dimension — a view's `Dimension` column sourced from a join is usually an
`Attribute` in the schema, because the join path selects it with `any_value`
precisely because attributes should agree. So:

- **A join's keys must be carried by some grain of the joined dataset.** Mirror
  the compiler's own test (`compile.rs:703-709`): some `g` in
  `joined_ds.grains()` for which every `k` in `join.on` satisfies
  `joined_ds.carries(g, dims.base_column(k))`. Skip this check when the join's
  dataset is unknown — that is already an error and a second diagnostic about
  the same line is noise.
- **A `Measure` column must be a measure of the primary dataset.** The compiler
  looks it up in the primary dataset only (`compile.rs:426`), so a measure
  sourced from a join is unhonourable too. Error when the primary dataset does
  not have it, or has it with a role other than `ColumnRole::Measure { .. }`.
  This is the case that today reaches `_ => Aggregate::Sum` and produces a
  plausible wrong total from a grain-bearing attribute.
- **A `Dimension` column must be reachable.** Either it is in `view.grouping`,
  or it is a column of some joined dataset. Check against `view.grouping`, NOT
  the compiler's `materialized` prefix: a grouping column deeper than the
  query's `max_depth` is legitimately absent at that depth and is not a
  configuration error.
- **`Derived` columns are unchanged.** Their SQL is the compiler's business.

Each failure is an `Error` that refuses the view (Task 1 does the refusing),
unless the join or column carries `required = false`, in which case it is an
`Info` diagnostic naming what was dropped.

- [ ] **Step 1: Write the failing tests**

Five cases, one test each so a failure names itself. `crates/geode-core/src/view.rs`'s
tests module already has everything you need: `doc(text)`, `dimensions(text)`,
`schema()` and the `SAMPLE` view. Two preliminaries:

**First, extend `schema()` additively** — it has no attribute on the primary
dataset, and the measure-over-an-attribute case is the one that produces a
plausible wrong total. Append to its text, changing nothing already there:

```toml
[risk_snapshot.columns.desk_name]
type = "utf8"
role = "attribute"
grain = "underlying"
```

**Second, confirm `SAMPLE` still validates clean** after your checks: its `book`
column is a grouped dimension, `delta01` a real measure, `delta_per_vega`
derived. If `SAMPLE` starts failing, your rule is too strict — that fixture is
a correct view.

```rust
    /// A join key no grain of the joined dataset carries cannot be honoured:
    /// the compiler finds no grain to read and drops the whole join, so every
    /// column it was to supply paints blank.
    #[test]
    fn a_join_whose_keys_no_grain_carries_refuses_the_view() {
        let text = r#"
[v]
dataset = "risk_snapshot"
grouping = ["book"]

[[v.joins]]
dataset = "instrument_ref"
on = ["book"]

[[v.columns]]
name = "book"
kind = "dimension"
"#;
        let (views, _) = ViewSpec::from_doc(&doc(text));
        let diags = views[0].validate(&schema(), &dimensions(""));
        let errors: Vec<&Diagnostic> = diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect();
        assert_eq!(errors.len(), 1, "{diags:?}");
        assert!(
            errors[0].message.contains("book") && errors[0].message.contains("instrument_ref"),
            "the diagnostic must name the key and the dataset: {}",
            errors[0].message
        );
    }

    /// The same join declared optional is dropped and said so, never refused.
    #[test]
    fn an_optional_join_whose_keys_no_grain_carries_is_dropped_with_an_info() {
        // Same document as above plus `required = false` on the join.
        // Assert: no Error, exactly one Info, and its message names the join.
    }

    /// An attribute repeats across every row of its grain, so summing it
    /// yields a total that looks right and is not. This is the reason the
    /// `kind` default of "measure" is the dangerous one.
    #[test]
    fn a_measure_column_over_an_attribute_refuses_the_view() {
        // `desk_name` is an attribute of risk_snapshot at grain underlying.
        // Declare it `kind = "measure"` (or omit kind, which means measure).
        // Assert: one Error whose message names `desk_name`.
    }

    /// Selected nowhere: the spine carries only grouping columns, aggregates,
    /// joins and derived expressions, so this column is absent, not NULL.
    #[test]
    fn a_dimension_column_neither_grouped_nor_joined_refuses_the_view() {
        // `counterparty` is a dimension of risk_snapshot, absent from the
        // grouping and from instrument_ref. Assert one Error naming it.
    }

    /// The opt-out on a column, with the drop named.
    #[test]
    fn an_optional_unreachable_column_is_dropped_with_an_info_naming_it() {
        // The same column with `required = false`: no Error, one Info naming it.
    }
```

Write the four abbreviated bodies out in full following the first one's shape —
same `doc`/`validate` calls, same severity partition, same "the message names
it" assertion. Every one of the five asserts BOTH the severity and that the
message names the column or key: a test that checks only severity does not
guard the contract that the diagnostic is the remedy.

- [ ] **Step 2: Run them to confirm they fail**

Run: `cargo test -p geode-core --lib view::tests 2>&1 | tail -20`
Expected: the five new tests FAIL; everything else passes.

- [ ] **Step 3: Implement the checks**

Extend `validate`. Add an `info` closure beside the existing `bad` one, and a
small helper that pushes one or the other by `required`:

```rust
        // A required declaration that cannot be honoured refuses the view; an
        // optional one is dropped and said so. The author chose which, so
        // nothing here guesses per kind.
        let mut report = |required: bool, message: String| {
            diags.push(if required { bad(message) } else { info(message) });
        };
```

Then the join-key check inside the existing `for j in &self.joins` loop (after
the unknown-dataset branch, in an `else`), and the column checks inside the
existing `for c in &self.columns` loop's `other` arm, after the current
unknown-column branch. Keep the existing `continue` for derived dimensions.

Update the doc comment at `:343-346`: it must no longer say validation skips
join keys and column roles, and it should say what is still left to the compiler
(derived SQL and sort keys).

- [ ] **Step 4: Run the tests, then the whole workspace**

```bash
cargo test -p geode-core --lib view::tests
cargo test -p geode-core every_shipped_views_document_validates_clean
cargo test --workspace
```
Expected: PASS, including Task 1's shipped-config test. **If the shipped-config
test now fails, the shipped configuration has a real defect — report it, do not
loosen either the check or the test.**

- [ ] **Step 5: Gates and commit**

```bash
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
git add -A
git commit -m "feat(core): validate join keys and column reachability at load

A measure declared over a grain-bearing attribute was summed into a
plausible wrong total; an unreachable dimension column was selected nowhere
and painted blank. Both now refuse the view, or drop with an Info when the
declaration says required = false."
```

---

### Task 4: The compiler stops guessing

**Files:**
- Modify: `crates/geode-data/src/query/compile.rs` — the join loop
  (`:692-709`) and the aggregate list (`:424-439`)
- Test: `crates/geode-data/src/query/compile.rs` tests module

**Interfaces:**
- Consumes: Task 3's checks, which are now the gate this task relies on.
- Produces: no new signature.

- [ ] **Step 1: Write the failing test**

A compiled view whose join names an unknown dataset must now return `Err`
naming it, rather than compiling with the join dropped. The module's
`compile_with(&store, &view)` helper (`grep -n "fn compile_with"`) is the way in;
it returns the `CompiledQuery`, so a test needing the error will want the
underlying call it wraps — read the helper before writing the test and use
whichever of the two gives you the `Result`:

```rust
    #[test]
    fn a_join_the_compiler_cannot_honour_is_an_error_not_a_silent_drop() {
        // Validation is the gate now, so reaching the compiler with an
        // unhonourable join is a bug in the caller, not a configuration
        // mistake to absorb.
    }
```

Write the body against that helper. Also assert the middle `continue` still
holds: a join whose key is not on the materialized spine still compiles and
yields NULL columns, because that is a depth fact rather than a configuration
error.

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test -p geode-data a_join_the_compiler_cannot_honour`
Expected: FAIL — it compiles instead of erroring.

- [ ] **Step 3: Convert the two configuration-error arms**

```rust
        let Some(joined_ds) = schema.dataset(&join.dataset) else {
            // Validation refuses this view before any query reaches here, so
            // arriving with an unknown join dataset means the caller skipped
            // the gate. Dropping the join silently is what made the joined
            // columns paint blank forever.
            return Err(compile_error(
                view,
                format!("join names unknown dataset '{}'", join.dataset),
            ));
        };
```

Keep the middle `continue` exactly as it is, comment intact. Convert the
`joined_grain` arm the same way, with a message naming the keys.

- [ ] **Step 4: Restructure the `Sum` fallback away**

`_ => Aggregate::Sum` is unreachable once a measure column must be a measure.
Do not leave it as a trap. Make the aggregate come from the role at the point
the measure list is built, so a non-measure cannot reach the aggregate at all:

```rust
        let measures: Vec<(&ColumnSpec, Aggregate)> = view
            .columns
            .iter()
            .filter_map(|c| match c {
                ViewColumn::Measure { name, .. } => ds.column(name),
                _ => None,
            })
            .filter_map(|c| match c.role {
                // The role carries the aggregate, so there is no default to
                // fall back to. Validation refuses a measure column that is
                // not a measure; anything else here would have been summed,
                // and a grain-bearing attribute repeats across its rows, so
                // the sum is plausible and wrong.
                ColumnRole::Measure { aggregate, .. } => Some((c, aggregate)),
                _ => None,
            })
            .filter(|(c, _)| c.grain() == Some(grain))
            .collect();
```

and take the aggregate from the tuple where `aggs` is built. Keep the
`measures.is_empty()` early return.

- [ ] **Step 5: Run and gate**

```bash
cargo test -p geode-data a_join_the_compiler_cannot_honour
cargo test -p geode-data
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "fix(data): the compiler refuses an unhonourable join instead of dropping it

Validation is the gate, so a configuration error reaching the compiler is a
bug rather than something to absorb. The Sum fallback is gone: the aggregate
now comes from the role where the measure list is built, so a non-measure
cannot reach it."
```

---

### Task 5: Harness entries and documentation

**Files:**
- Modify: `scripts/mutation-check.sh`
- Modify: `docs/current/data-path.md`, `docs/current/configuration.md`,
  `crates/geode-core/README.md`, `crates/geode-data/README.md`

**Preflight.** Run `zsh scripts/mutation-check.sh --anchors-only` FIRST and work
from its output, not from a prediction. This branch edits `view.rs`,
`compile.rs` and `service.rs`, all of which carry many entries, and it both
moves lines and ADDS text that existing anchors may already match — the second
class fails the gate exactly as staleness does and is easy to miss.

- [ ] **Step 1: Validate every anchor before touching anything**

Run: `zsh scripts/mutation-check.sh --anchors-only`
Record the verbatim output in your report. Re-anchor everything it names.

- [ ] **Step 2: Add the entries**

Five, each with a comment saying what the mutation lets through in terms of what
the trader or the desk would see:

1. **`views: an error diagnostic refuses the query`** — `service.rs`, mutate the
   `refused_views.get(view)` early return away. Covered by Task 1's test.
2. **`views: open and reload agree on which views are refused`** — mutate the
   reload site to keep the previous `refused_views` instead of recomputing, so a
   view fixed in the config stays refused (or a newly broken one stays served).
   This needs a test that reloads; if none exists, write it in this task and say
   so.
3. **`views: a join's keys must be carried by a grain`** — `view.rs`, mutate the
   carriage check to `true`. Covered by Task 3's test.
4. **`views: a measure column must really be a measure`** — `view.rs`, mutate the
   role check to accept any role. Covered by Task 3's test.
5. **`views: required = false drops rather than refuses`** — `view.rs`, mutate
   `report` to always push `bad`, so an optional declaration refuses the view.
   Covered by Task 3's optional tests.

For each: run it by name and confirm `caught`, AND apply the mutation by hand to
confirm the named test fails on an ASSERTION, not a compile error. The harness
reads any non-zero cargo exit as `caught`, so a mutation that fails to compile
is a permanent false pass.

Rules while running it: commit first; FOREGROUND only; NEVER `--changed`; never
start an unfiltered run; if the script reports a lock held by another session,
wait and retry rather than killing anything.

- [ ] **Step 3: Update the guides**

Each sentence is a behaviour claim — verify it against source, not against
another document.

- `docs/current/data-path.md` — in the query section, that a view is validated
  at load and on reload, that a view with an error is refused by name when
  queried rather than compiled, and that a join or column may carry
  `required = false` to be dropped with an informational diagnostic instead.
  Say plainly that an absent column used to be the outcome and that a refusal
  replaced it.
- `docs/current/configuration.md` — the `required` key on a view join and a view
  column, defaulting to true, in whatever form that guide documents view keys.
  Note the upgrade consequence: a column written with just a name defaults to
  `kind = "measure"`, so a non-measure written that way now refuses its view.
- `crates/geode-core/README.md` — that `ViewSpec::validate` is the gate, and what
  it still leaves to the compiler.
- `crates/geode-data/README.md` — that `query` refuses a view validation
  rejected, and that the compiler's remaining `continue` is a depth fact rather
  than a configuration error.

- [ ] **Step 4: Final gate**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo bench --workspace --no-run
cargo check -p geode-shell --features test-support --all-targets
zsh scripts/mutation-check.sh --anchors-only
```

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "test(core): harness entries for view strictness, and docs

features of the refusal, the required opt-out, and the upgrade consequence of
kind defaulting to measure."
```

---

## Rulings recorded while planning

Carry these into the branch ledger.

1. **Only two of the spec's three checks are new.** `validate` already reports a
   join naming an unknown dataset (`view.rs:362-366`). Task 3 adds join-key
   carriage and column reachability/role. Converting the compiler's
   corresponding `continue` to an error remains in scope, because a silent drop
   there is still a silent drop.
2. **Refusal has to be built, and it is Task 1 rather than a step of Task 3.**
   `validate`'s Error diagnostics were only ever collected; `query` compiled the
   view regardless. Without refusal the new checks change nothing observable.
3. **The blast radius is wider than §5.3 says, and the ruling still covers it.**
   Refusal also enforces the three Error diagnostics `validate` already
   produces, so a view naming an unknown column stops opening — not only one
   with a mis-declared `kind`. Flagged rather than absorbed; the user's ruling
   was to keep it strict and revise if practice argues otherwise.
4. **`service.rs:1295-1296` is false today** ("Report and skip broken views")
   and is corrected in Task 1 rather than left to the docs task, because a
   comment asserting behaviour the code does not have is worse than none.
5. **Check 3 is role-matching for a measure and reachability for a dimension**,
   not "kind matches role" for both. A view's `Dimension` column sourced from a
   join is usually an `Attribute` in the schema — the join path selects it with
   `any_value` precisely because attributes should agree — so requiring
   `role == Dimension` would refuse correct configurations.
6. **Dimension reachability is checked against `view.grouping`, not the
   compiler's `materialized` prefix.** A grouping column deeper than a query's
   `max_depth` is legitimately absent at that depth; only a column in neither
   the grouping nor a joined dataset is a configuration error.
7. **A fourth silent case, not in the spec, is folded into check 3:** a
   `Dimension` column neither grouped nor joined is selected nowhere and paints
   blank. Same defect shape as the two the spec names.
8. **`ViewColumn` gains a real `required` field plus constructors, not a side
   map.** The spec puts the declaration on the form it describes; constructors
   make the 35-site churn one mechanical pass without weakening the type.
9. **The `Sum` fallback is restructured away rather than left unreachable.** The
   aggregate comes from the role where the measure list is built, so a
   non-measure cannot reach an aggregate at all.

---

## Verified before execution, so no task has to rediscover it

- **The demo configuration is already clean under every new check**, checked
  mechanically against `examples/demo-config/datasets.toml`: it has exactly two
  views (`tree`, `wide`), **zero** `kind = "dimension"` columns, and **zero**
  measure-kind columns whose dataset role is not `measure` — including all 44
  that rely on the `kind` default. So spec §5.3's obligation holds before a line
  is written, and Task 1's shipped-config test should pass the moment it exists.
  If it does not, something else changed; report it rather than loosening it.
- `ViewSpec::validate`'s three existing Error diagnostics are unknown dataset,
  unknown column, and a grouping that shadows a real column. Those are the ones
  Task 1's refusal starts enforcing.
- `view.rs`'s tests module already provides `doc(text)`, `dimensions(text)`,
  `schema()` and `SAMPLE`; `SAMPLE` validates clean under the new rules.
- `service.rs`'s tests module already provides `service()`, the `params(...)`
  helper, and `a_misconfigured_view_is_a_diagnostic_at_open_not_a_binder_error_later`,
  which is the fixture Task 1's test extends.
- `DatasetSpec::carries` (`crates/geode-core/src/schema/mod.rs:119`) and
  `DerivedDimensions::base_column` (`crates/geode-core/src/dimensions.rs:38`) are
  both public, so Task 3's join-key check needs no new core API and no I/O.
