# Dividend Schedule (Phase 2) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The dividend schedule panel — the second market-data document kind — and, on the way, per-row dates and text in the document family, a per-column typed flat panel layout, and row insert/delete in the market-data draft on both layouts (CVI included).

**Architecture:** The document family's `value` role widens to `date`/`utf8` (storage already carries both). `PanelSpec` declares each flat column with its own type, format, choices and `required` flag, and names the row axis's identity (`Typed` for CVI's term, `Minted` for a dividend id). `MatrixModel` cells carry a typed `Value` and a per-column `CellKind` that the delegate, the editor door and the nudge all read; a cell commit patches one cell instead of rebuilding. `Draft` gains `rows: BTreeMap<label, RowEdit>` beside `edits`/`attrs` — inserted rows are spliced into the model after their anchor, deleted rows stay painted struck through — carried by label through rebase, session and parking. `DividendKind` (quick-xml, CVI's mould), `DividendGenerator` (seeded) and a multi-producer demo bus feed it; `DIVIDEND` is a second `PanelSpec` registered as a second factory sharing the `marketdata` vocabulary.

**Tech Stack:** Rust, gpui / gpui-component 0.6.2 (pinned), DuckDB via `geode-data`, `quick-xml`, `chrono`, `rand` (`StdRng`), criterion benches, `TestAppContext` window tests, `scripts/mutation-check.sh`.

**Spec:** `docs/superpowers/specs/2026-09-19-geode-dividend-schedule-and-choice-design.md` §2 (rulings 1, 2, 3, 6, 7), §4, §5, §6, §7, §8, §9. Phase 1 (§3, the choice core and `dialog::choice_rows`) is merged and is what the panel's `Choice` cell uses.

## Global Constraints

- `geode-data` depends on neither `geode-documents` nor `geode-demo-data`; only `geode-app` sees a document kind. `geode-marketdata` may depend on `geode-shell` (it does) but never on `geode-blotter`.
- A document `role = "value"` column may be `f64`, `i64`, `date` or `utf8`; `timestamp`/`bool` stay refused (`document::Column`/`Value` do not carry them).
- A flat panel names its columns explicitly (`Columns::Values(&[ValueColumn])`, paint order); a document value column the spec does not list is **refused** by the build, never painted unlabelled. The pivot still requires exactly one numeric cell column.
- Nothing in `render` formats or allocates per cell per frame beyond today's baseline: every cell's text is prepared in `MatrixModel::build`/`patch_cell`; `SharedString`s are cloned, not built, in `render_td`.
- A cell commit calls `MatrixModel::patch_cell` on both layouts; only a row insert/delete and a delivery rebuild.
- `Draft.rows` is keyed by row label; an inserted row's cell edits live in `RowEdit::Inserted.cells` keyed by column label, never in `edits`. Rebase rules (§5.1): a `Deleted` whose label vanished is dropped and named; an `Inserted` whose label the newer document carries is dropped and named as a conflict; a vanished `after` anchor re-anchors to the top and is named.
- A deleted document row stays painted (struck through) until `:revert`/upload (ruling 6); an `Inserted` row `d d`-ed is dropped outright.
- Minted ids are `new-<n>`, smallest unused `n`, never reused within a draft; `DividendKind::parse` refuses an upstream id beginning `new-`.
- Row verbs (`o`, `shift+o`, `d d`) and `space`/`shift+space` on a choice cell are refused while `Behind`, while the model is empty, and from the attribute strip.
- Every editor closes through `close_editor` / every popup through `close_popup_with_window` — blur only when the own field is focused, then drop (CLAUDE.md's insert-mode rule).
- `status`'s closed set is `estimated · declared · paid · cancelled`, declared ONCE as `DividendKind::STATUSES` in `crates/geode-documents/src/dividend.rs`; the spec's `choices` references it. Wire tag names are an assumption until the XSD (one table).
- The demo bus publishes every key of every producer once at start, then one key per `cadence ± jitter` round-robin across producers, so the overall generation rate stays ~one per 5 s.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo bench --workspace --no-run` and `zsh scripts/mutation-check.sh --anchors-only` pass before merge; both macOS and Windows build. Never dispatch `mutation-check.sh --changed` from a task — only `--anchors-only` and the task's own filtered entries.
- Commit after every task with the attribution lines from the session's system reminder. Commit BEFORE running the mutation harness.

---

### Task 1: `role = "value"` accepts `date` and `utf8`

**Files:**
- Modify: `crates/geode-core/src/schema/mod.rs` (`validate_document`'s "a value must be f64 or i64" block, ~line 560; the doc comment on `Column`/`Value` coverage just below it)
- Test: the same file's `mod tests` (beside the existing `cvi_params` value-type tests, ~line 1533–1560)

**Interfaces:**
- Produces: nothing new in code — the loader accepts `[ds.columns.x]\ntype = "date"\nrole = "value"` and `type = "utf8"\nrole = "value"`; still errors `datasets.{ds}.columns.{c}.type` and drops the column for `timestamp`/`bool`.

- [ ] **Step 1: Write the failing tests**

In `crates/geode-core/src/schema/mod.rs`'s tests, next to the existing test that asserts a non-numeric value is dropped (grep `a value must be f64 or i64`):

```rust
    /// Spec 2026-09-19 §4.1 (ruling 7): a value is "a per-row fact that
    /// is not identity" and may be a date or text — a dividend's ex date
    /// and status. `timestamp`/`bool` stay refused: `document::Column`
    /// carries neither.
    #[test]
    fn a_document_value_may_be_a_date_or_text() {
        let doc = cvi_doc_with(
            "[cvi_params.columns.ex]\ntype = \"date\"\nrole = \"value\"\n\
             [cvi_params.columns.status]\ntype = \"utf8\"\nrole = \"value\"",
        );
        let (schema, diags) = parse_schema(&doc);
        let ds = schema.dataset("cvi_params").expect("the dataset survives");
        assert!(ds.columns.iter().any(|c| c.name == "ex" && c.role == ColumnRole::Value));
        assert!(ds.columns.iter().any(|c| c.name == "status" && c.role == ColumnRole::Value));
        assert!(
            !diags.iter().any(|d| d.message.contains("a value must be")),
            "{diags:?}"
        );
    }

    #[test]
    fn a_document_value_may_not_be_a_timestamp_or_bool() {
        let doc = cvi_doc_with(
            "[cvi_params.columns.when]\ntype = \"timestamp\"\nrole = \"value\"\n\
             [cvi_params.columns.flag]\ntype = \"bool\"\nrole = \"value\"",
        );
        let (schema, diags) = parse_schema(&doc);
        let ds = schema.dataset("cvi_params").expect("the dataset survives");
        assert!(!ds.columns.iter().any(|c| c.name == "when"));
        assert!(!ds.columns.iter().any(|c| c.name == "flag"));
        let errors: Vec<_> = diags
            .iter()
            .filter(|d| d.message.contains("a value must be f64, i64, date or utf8"))
            .collect();
        assert_eq!(errors.len(), 2, "{diags:?}");
        assert!(errors.iter().all(|d| d.path.as_deref().is_some_and(|p| p.ends_with(".type"))));
    }
```

(`cvi_doc_with` and `parse_schema` are whatever the neighbouring tests use to build a `cvi_params` document with extra column tables and run the loader — read those tests and use their exact helpers; if they build the TOML inline, do the same.)

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-core a_document_value_may 2>&1 | tail -15`
Expected: `a_document_value_may_be_a_date_or_text` FAILS (the date/utf8 columns are dropped with "a value must be f64 or i64"); the second fails on the message text.

- [ ] **Step 3: Widen the rule**

Replace the block:

```rust
    // A value is a per-row fact that is not identity (spec 2026-09-19
    // §4.1, ruling 7): a number, a date or text — a dividend's amount,
    // its ex date, its status. `timestamp`/`bool` are refused because
    // `geode_core::document::Column`/`Value` — the shapes a parsed
    // document arrives in — carry neither, so such a column could be
    // declared but never filled.
    let non_value: Vec<String> = ds
        .columns
        .iter()
        .filter(|c| {
            c.role == ColumnRole::Value
                && !matches!(
                    c.ty,
                    ColumnType::F64 | ColumnType::I64 | ColumnType::Date | ColumnType::Utf8
                )
        })
        .map(|c| c.name.clone())
        .collect();
    for c in &non_value {
        diags.push(err(
            format!("dataset '{name}' column '{c}': a value must be f64, i64, date or utf8 — column dropped"),
            format!("datasets.{name}.columns.{c}.type"),
        ));
    }
    ds.columns.retain(|c| !non_value.contains(&c.name));
```

Update the doc comment that follows (it says values are held "to the stricter f64/i64 rule") to say the value rule is now the same four types the axis/attribute rule allows. Update any existing test asserting the old message (grep `must be f64 or i64`).

- [ ] **Step 4: Run the tests**

Run: `cargo test -p geode-core 2>&1 | tail -3` — expected all pass. Then `cargo test -p geode-data 2>&1 | tail -3` (the document-family tests must be unaffected).

- [ ] **Step 5: Harness entry and commit**

Append to `scripts/mutation-check.sh` after the existing `schema: the document family` block:

```bash
# Spec 2026-09-19 §4.1: a document value may be a date or text. Mutated
# back to the numeric-only rule, a dividend's ex date is dropped at load.
run_mutation "schema: a document value may be a date or text" \
  crates/geode-core/src/schema/mod.rs \
  '                    ColumnType::F64 | ColumnType::I64 | ColumnType::Date | ColumnType::Utf8' \
  '                    ColumnType::F64 | ColumnType::I64' \
  geode-core a_document_value_may_be_a_date_or_text
```

```bash
git add crates/geode-core scripts/mutation-check.sh
git commit -m "schema: a document value may be a date or text (spec §4.1)"
zsh scripts/mutation-check.sh --anchors-only && zsh scripts/mutation-check.sh "a document value may be"
```

---

### Task 2: `PanelSpec` — `ValueColumn`, `Columns::Values(&[..])`, `RowAxis`

**Files:**
- Modify: `crates/geode-marketdata/src/core/spec.rs`
- Modify: every `spec.rows` read (`crates/geode-marketdata/src/core/matrix.rs`, `tile.rs`, `delegate.rs`, benches — 9 sites) → `spec.rows.column`; every `Columns::Values` construction in tests/benches (8 sites) → `Columns::Values(&[..])`
- Test: `spec.rs` tests

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum RowIdentity { Typed(ColumnType), Minted }
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub struct RowAxis { pub column: &'static str, pub identity: RowIdentity }
  #[derive(Debug, Clone, PartialEq)]
  pub struct ValueColumn {
      pub column: &'static str,
      pub label: &'static str,
      pub ty: ColumnType,
      pub format: ColumnFormat,
      pub choices: Option<&'static [&'static str]>,
      pub required: bool,
  }
  #[derive(Debug, Clone, PartialEq)]
  pub enum Columns { Axis(&'static str), Values(&'static [ValueColumn]) }
  pub struct PanelSpec { ..., pub rows: RowAxis, pub columns: Columns, ... }
  impl PanelSpec {
      pub fn names(&self, column: &str) -> bool;           // now also true for a ValueColumn.column
      pub fn value_column(&self, column: &str) -> Option<&ValueColumn>;   // Columns::Values only
      pub fn flat_columns(&self) -> &'static [ValueColumn]; // &[] under Columns::Axis
  }
  pub const CVI: PanelSpec  // rows: RowAxis { column: "term", identity: RowIdentity::Typed(ColumnType::Date) }
  ```
  `Columns` loses `Copy` (a slice of non-`Copy` `ValueColumn`s).

- [ ] **Step 1: Write the failing tests**

In `spec.rs`'s `mod tests`:

```rust
    /// §4.2: a flat panel names its columns; `names` covers them, and
    /// `value_column` answers each by name so the build can refuse a
    /// value the spec does not list rather than paint it unlabelled.
    #[test]
    fn a_flat_spec_names_its_value_columns() {
        const FLAT: PanelSpec = PanelSpec {
            kind: "flat",
            title: "Flat",
            dataset: "d",
            document: "d",
            rows: RowAxis { column: "id", identity: RowIdentity::Minted },
            columns: Columns::Values(&[
                ValueColumn {
                    column: "amount",
                    label: "amount",
                    ty: ColumnType::F64,
                    format: CVI_FORMAT,
                    choices: None,
                    required: true,
                },
                ValueColumn {
                    column: "status",
                    label: "status",
                    ty: ColumnType::Utf8,
                    format: CVI_FORMAT,
                    choices: Some(&["a", "b"]),
                    required: false,
                },
            ]),
            header: &[],
            slice_values: &[],
            value_type: ColumnType::F64,
            format: CVI_FORMAT,
            actions: &[],
        };
        assert!(FLAT.names("id"));
        assert!(FLAT.names("amount"));
        assert!(FLAT.names("status"));
        assert!(!FLAT.names("underlying_ref"));
        assert_eq!(FLAT.value_column("status").unwrap().choices, Some(&["a", "b"][..]));
        assert!(FLAT.value_column("nope").is_none());
        assert_eq!(FLAT.flat_columns().len(), 2);
        assert!(CVI.flat_columns().is_empty());
        assert_eq!(CVI.rows.identity, RowIdentity::Typed(ColumnType::Date));
    }
```

- [ ] **Step 2: Run to verify it fails** — `cargo test -p geode-marketdata a_flat_spec_names` → compile error (`RowAxis`, `ValueColumn` missing).

- [ ] **Step 3: Implement**

In `spec.rs`, add the three types (docs in the crate's voice: `ValueColumn` is "one flat column: what it reads, how it paints, how it is edited, whether an inserted row must fill it"; `RowIdentity` is "who names a new row — the trader (`Typed`, the row-label editor opens on insert) or the panel (`Minted`, `new-<n>`)"), change `Columns::Values` to carry `&'static [ValueColumn]`, change `PanelSpec.rows` to `RowAxis`, drop `Copy` from `Columns`' derive, and extend `names`:

```rust
    pub fn names(&self, column: &str) -> bool {
        self.rows.column == column
            || matches!(self.columns, Columns::Axis(a) if a == column)
            || self.value_column(column).is_some()
            || self.header.iter().any(|h| h.column == column)
            || self.slice_value(column).is_some()
    }

    /// The flat column this spec paints from `column`, if the layout is
    /// flat and lists it.
    pub fn value_column(&self, column: &str) -> Option<&ValueColumn> {
        self.flat_columns().iter().find(|c| c.column == column)
    }

    /// The flat layout's columns in paint order; empty under a pivot.
    pub fn flat_columns(&self) -> &'static [ValueColumn] {
        match self.columns {
            Columns::Values(cols) => cols,
            Columns::Axis(_) => &[],
        }
    }
```

`CVI.rows` becomes `RowAxis { column: "term", identity: RowIdentity::Typed(ColumnType::Date) }`. Then fix every compile error: `spec.rows` → `spec.rows.column` (9 sites), `Columns::Values` → `Columns::Values(&[])` in existing tests/benches where the test's snapshot has one value column named by the test — give each such test a `ValueColumn` entry per value column its snapshot carries (name = the snapshot's column name, `ty: F64`, `format: CVI_FORMAT`-like, `choices: None`, `required: true`), since Task 3 will refuse an unlisted value. Update the existing `the_cvi_spec_names_its_own_axes…` test if `names("param")` changes (it does not — `param` is CVI's pivot value, still unnamed).

- [ ] **Step 4: Run** — `cargo test -p geode-marketdata 2>&1 | tail -3` and `cargo bench -p geode-marketdata --no-run` — all green.

- [ ] **Step 5: Commit** — `git commit -m "marketdata: PanelSpec names its flat columns and its row axis's identity (spec §4.2)"`.

---

### Task 3: Typed cells — `CellKind`, `Value` in `Cell` and `Draft::edits`

**Files:**
- Modify: `crates/geode-marketdata/src/core/matrix.rs` (`Cell`, `MatrixModel`, `flatten`, `pivot`, `cell_of`, `label_of` unchanged)
- Modify: `crates/geode-marketdata/src/core/draft.rs` (`edits`, `set`, `bump`, `rebase`, `to_toml`, `from_toml`, `count_phrase` unchanged)
- Modify: `crates/geode-marketdata/src/tile.rs` (`commit_cell_edit`'s `parse_cell` call, `bump`, `yank_text`, `nudge`'s cell arm), `delegate.rs` (nothing structural — `cell.text` stays a `SharedString`), `benches/matrix.rs` (`Draft::set` calls)
- Test: `matrix.rs` and `draft.rs` tests

**Interfaces:**
- Produces:
  ```rust
  // matrix.rs
  #[derive(Debug, Clone, PartialEq)]
  pub enum CellKind { Number(ColumnFormat), Date, Text, Choice(&'static [&'static str]) }
  pub struct Cell { pub text: SharedString, pub value: Option<Value>, pub edited: bool, pub sent: bool, pub cell_ref: (usize, usize) }
  pub struct MatrixModel { ..., pub column_kinds: Vec<CellKind>, ... }   // parallel to `columns`
  impl MatrixModel { pub fn kind_of(&self, col: usize) -> Option<&CellKind>; }
  pub fn cell_text(value: &Value, kind: &CellKind) -> String;   // the one formatter: Number → format_number; Date → %Y-%m-%d; Text/Choice → the string
  // draft.rs
  pub edits: BTreeMap<(usize, usize), Value>
  pub fn set(&mut self, cell: (usize, usize), labels: (String, String), value: Value, base: &str)
  pub fn bump(...)  // unchanged signature; adds to Number cells only, counts skipped cells in its Err
  pub fn numeric_edit(&self, cell) -> Option<f64>   // Value::F64/I64 as f64, else None
  ```
  `to_toml` writes a cell edit as `[row, col, value]` where `value` is a TOML float/integer for numbers, and `{ type = "date", value = "YYYY-MM-DD" }` / `{ type = "text", value = "…" }` tables for the other two; `from_toml` reads all three shapes (a bare string is refused/skipped — never guessed).

- [ ] **Step 1: Write the failing tests**

`matrix.rs` tests (use the file's existing `TestColumn`/snapshot builders; add a `Date` and a `Utf8` column to the flat fixture — the file's comment says `TestColumn` has no date arm, so add one that builds a `Date32` array exactly as `document_of` in `tile.rs`'s tests does):

```rust
    /// §4.3: a flat panel's cells are typed per column — a date paints
    /// ISO, text paints itself, a number paints through its column's
    /// own format — and `column_kinds` runs parallel to `columns`.
    #[test]
    fn a_flat_model_types_each_column_by_its_spec() {
        let snapshot = schedule_snapshot(&[
            ("D1", "2026-12-18", 1.25, "declared"),
            ("D2", "2027-03-19", 0.5, "estimated"),
        ]);
        let model = MatrixModel::build(&snapshot, &SCHEDULE, &Draft::default()).unwrap();
        assert_eq!(model.columns, ["ex", "amount", "status"]);
        assert!(matches!(model.column_kinds[0], CellKind::Date));
        assert!(matches!(model.column_kinds[1], CellKind::Number(_)));
        assert!(matches!(model.column_kinds[2], CellKind::Choice(_)));
        assert_eq!(model.rows[0].cells[0].text.as_ref(), "2026-12-18");
        assert_eq!(model.rows[0].cells[1].text.as_ref(), "1.2500");
        assert_eq!(model.rows[0].cells[2].text.as_ref(), "declared");
        assert_eq!(model.rows[1].cells[0].value, Some(Value::Date(date(2027, 3, 19))));
    }

    /// A value column the spec does not list is refused, never painted
    /// unlabelled: a schema drifting under a spec is reported.
    #[test]
    fn a_flat_model_refuses_a_value_column_the_spec_does_not_list() {
        let snapshot = schedule_snapshot_with_extra_value("bonus");
        let err = MatrixModel::build(&snapshot, &SCHEDULE, &Draft::default()).unwrap_err();
        assert!(err.contains("'bonus'") && err.contains("does not declare"), "{err}");
    }

    /// The pivot's one cell column must be numeric.
    #[test]
    fn a_pivot_refuses_a_non_numeric_value_column() {
        let snapshot = cvi_with_text_param();
        let err = MatrixModel::build(&snapshot, &CVI, &Draft::default()).unwrap_err();
        assert!(err.contains("numeric"), "{err}");
    }

    /// A typed edit paints in its column's own kind.
    #[test]
    fn a_typed_edit_paints_by_its_columns_kind() {
        let snapshot = schedule_snapshot(&[("D1", "2026-12-18", 1.25, "declared")]);
        let mut draft = Draft::default();
        draft.set((0, 2), ("D1".into(), "status".into()), Value::Utf8("paid".into()), "t0");
        draft.set((0, 0), ("D1".into(), "ex".into()), Value::Date(date(2026, 12, 20)), "t0");
        let model = MatrixModel::build(&snapshot, &SCHEDULE, &draft).unwrap();
        assert_eq!(model.rows[0].cells[2].text.as_ref(), "paid");
        assert!(model.rows[0].cells[2].edited);
        assert_eq!(model.rows[0].cells[0].text.as_ref(), "2026-12-20");
    }
```

Define a `SCHEDULE: PanelSpec` in the test module (kind `"sched"`, rows `RowAxis { column: "dividend_id", identity: Minted }`, columns `ex_date` Date "ex" required / `amount` F64 "amount" precision 4 required / `status` Utf8 "status" choices `["estimated","declared","paid","cancelled"]` required), and `schedule_snapshot(rows: &[(&str, &str, f64, &str)])` building a snapshot with columns `underlying_ref` (Additive), `dividend_id` (Additive), `ex_date` Date (DeterminedNonAdditive), `amount` F64 (DeterminedNonAdditive), `status` Utf8 (DeterminedNonAdditive) — copy the shape `tile.rs`'s `document_of` uses for `cvi`.

`draft.rs` tests:

```rust
    #[test]
    fn typed_edits_round_trip_through_toml_with_a_type_tag() {
        let mut draft = Draft::default();
        draft.set((0, 0), ("D1".into(), "ex".into()), Value::Date(date(2026, 12, 20)), "t0");
        draft.set((0, 1), ("D1".into(), "amount".into()), Value::F64(1.5), "t0");
        draft.set((0, 2), ("D1".into(), "status".into()), Value::Utf8("paid".into()), "t0");
        let t = draft.to_toml();
        let back = Draft::from_toml(&t);
        let values: Vec<_> = back.edits.values().cloned().collect();
        assert!(values.contains(&Value::Date(date(2026, 12, 20))));
        assert!(values.contains(&Value::F64(1.5)));
        assert!(values.contains(&Value::Utf8("paid".into())));
        // A date is a tagged table, never a bare string a text edit could
        // be confused with.
        let text = toml::to_string(&t).unwrap();
        assert!(text.contains("type = \"date\""), "{text}");
    }

    #[test]
    fn bump_adds_to_number_cells_only() {
        // model with one Number column and one Text column, one row
        let (model, mut draft) = flat_model_with_text_column();
        let res = draft.bump(&model, 1.0, BumpAxis::Row, 0, "t0");
        assert!(res.is_ok());
        assert_eq!(draft.numeric_edit((0, 0)), Some(2.0));
        assert!(draft.edits.get(&(0, 1)).is_none(), "the text cell is skipped");
    }
```

(Read `Draft::bump`'s current signature and mirror it exactly; the test above assumes `(model, delta, axis, index, base)` — correct it to the real one.)

- [ ] **Step 2: Run to verify they fail** — `cargo test -p geode-marketdata typed_edits_round_trip a_flat_model_types` → compile errors.

- [ ] **Step 3: Implement**

`matrix.rs`:
- Add `CellKind`, `column_kinds: Vec<CellKind>` on `MatrixModel` (`empty()` leaves it empty), `kind_of`, and `pub fn cell_text(value: &Value, kind: &CellKind) -> String`:
  ```rust
  pub fn cell_text(value: &Value, kind: &CellKind) -> String {
      match (kind, value) {
          (CellKind::Number(format), Value::F64(v)) => format_number(*v, format).text,
          (CellKind::Number(format), Value::I64(v)) => format_number(*v as f64, format).text,
          (_, Value::Date(d)) => d.format("%Y-%m-%d").to_string(),
          (_, Value::Utf8(s)) => s.clone(),
          // A number in a non-number column: spelled plainly rather than
          // through a format the column does not have.
          (_, Value::F64(v)) => format!("{v}"),
          (_, Value::I64(v)) => v.to_string(),
      }
  }
  ```
- `pivot`: `column_kinds` = `Number(slice format)` per slice column then `Number(spec.format)` per ladder column; after choosing the one value column, refuse it unless the snapshot reads it as numeric — check `snapshot.meta_at(idx)`'s type through whatever accessor exists (`f64_at` succeeding on row 0 is NOT a type check; use the snapshot's column type if exposed, else `snapshot.display_at(idx, 0).is_some()` meaning "non-numeric" per `label_at`'s own rule) with the error `"a pivot's value column '{name}' must be numeric"`.
- `flatten`: iterate `spec.flat_columns()` in order; for each, find the snapshot column by name (refuse a missing one: `"the document has no '{c}' column"`); after that, any snapshot value column (`is_value`) not named by the spec is refused: `"the document carries a value column '{c}' this panel does not declare"`. Read each cell by kind: `Number` → `f64_at`; `Date`/`Text`/`Choice` → `display_at` (a `Date32` displays as `%Y-%m-%d` already — confirm by reading `Snapshot::display_at`; if it does not, read the date through the arrow array and format it). The `Cell.value` is `Some(Value::…)` of the right variant, `text` from `cell_text`.
- `cell_of` takes `&CellKind` instead of `&ColumnFormat`; an edited cell's text is `cell_text(edit, kind)`.

`draft.rs`: `edits: BTreeMap<(usize, usize), Value>`; `set` takes `Value`; `numeric_edit`; `bump` skips cells whose model kind is not `Number` (it takes the model — read the kind through `model.kind_of(col)`); `rebase` unchanged in shape (values cloned); `to_toml`/`from_toml` per the interface (a helper `value_to_toml(&Value) -> toml::Value` and `value_from_toml(&toml::Value) -> Option<Value>` shared with `attrs`, which already spell `Date` as a bare string — keep `attrs` as they are for session compatibility, only cell edits get the tag).

`tile.rs`: `commit_cell_edit` parses through `parse_cell(text, ty)` where `ty` is the column's declared type: `CellKind::Number(_)` → `self.spec.value_type` under a pivot or the `ValueColumn.ty` under flat (`Value::F64`/`I64` accordingly); the other kinds are Task 4's — for now `commit_cell_edit` on a non-`Number` cell refuses with `"not a numeric cell"` so this task stays green. `yank_text` reads `cell.text`. `nudge`'s cell arm reads the kind: a non-`Number` cell answers `"not a numeric cell"`. `bump`'s error naming skipped cells is surfaced as the notice.

- [ ] **Step 4: Run** — `cargo test -p geode-marketdata 2>&1 | tail -3`, `cargo bench -p geode-marketdata --no-run`, clippy on the crate.

- [ ] **Step 5: Harness + commit**

Two lines the implementation above must contain verbatim, so the entries can anchor on them — in `flatten`, after the spec's columns are resolved:

```rust
    for idx in 0..snapshot.columns() {
        let name = snapshot.meta_at(idx).map(|m| m.name.clone()).unwrap_or_default();
        if is_value(snapshot, idx) && spec.value_column(&name).is_none() {
            return Err(format!(
                "the document carries a value column '{name}' this panel does not declare"
            ));
        }
    }
```

and in `draft.rs`:

```rust
fn value_to_toml(value: &Value) -> toml::Value {
    match value {
        Value::F64(f) => toml::Value::Float(*f),
        Value::I64(i) => toml::Value::Integer(*i),
        Value::Date(d) => tagged("date", d.format("%Y-%m-%d").to_string()),
        Value::Utf8(s) => tagged("text", s.clone()),
    }
}

/// `{ type = "<ty>", value = "<value>" }` — a date and a text edit are
/// both strings on the wire, and only a tag keeps a restored
/// `2026-12-18` from reading back as text.
fn tagged(ty: &str, value: String) -> toml::Value {
    let mut t = toml::Table::new();
    t.insert("type".into(), toml::Value::String(ty.to_string()));
    t.insert("value".into(), toml::Value::String(value));
    toml::Value::Table(t)
}
```

```bash
# §4.2: an undeclared flat value column is refused, never painted.
run_mutation "matrix: an undeclared flat value column is refused" \
  crates/geode-marketdata/src/core/matrix.rs \
  '        if is_value(snapshot, idx) && spec.value_column(&name).is_none() {' \
  '        if is_value(snapshot, idx) && spec.value_column(&name).is_none() && false {' \
  geode-marketdata a_flat_model_refuses_a_value_column_the_spec_does_not_list

# §4.3: a cell edit round-trips typed; mutated to tag a date as text, the
# restored edit reads back as text.
run_mutation "draft: a date edit is tagged in the session" \
  crates/geode-marketdata/src/core/draft.rs \
  '        Value::Date(d) => tagged("date", d.format("%Y-%m-%d").to_string()),' \
  '        Value::Date(d) => tagged("text", d.format("%Y-%m-%d").to_string()),' \
  geode-marketdata typed_edits_round_trip_through_toml_with_a_type_tag
```

`--anchors-only` must report 0 stale/0 ambiguous. Commit: `"marketdata: typed cells — CellKind per column, Value in Cell and Draft::edits (spec §4.3)"`.

---

### Task 4: Editing by cell kind — Text and Date cells, `patch_cell`

**Files:**
- Modify: `crates/geode-marketdata/src/core/matrix.rs` (`patch_cell`)
- Modify: `crates/geode-marketdata/src/tile.rs` (`begin_edit`, `commit_edit`, `commit_cell_edit`, `nudge`)
- Modify: `crates/geode-marketdata/src/delegate.rs` (the editor slot paints a `DateField` in a cell too)
- Modify: `crates/geode-marketdata/src/header.rs` (`render_date_field` becomes reusable by the delegate — read it; if it needs the tile `Entity` for key routing, it already takes one)
- Test: `matrix.rs` (`patch_cell ≡ build`), `tile.rs` window tests

**Interfaces:**
- Consumes: Task 3's `CellKind`, `Value`, `cell_text`.
- Produces:
  ```rust
  impl MatrixModel {
      /// Re-prepare ONE cell from the snapshot and the draft — text, value, edited, sent — and answer
      /// whether the cell exists. Identical to `build`'s result for that cell (a test proves it).
      pub fn patch_cell(&mut self, row: usize, col: usize, snapshot: &Snapshot, spec: &PanelSpec, draft: &Draft) -> bool;
  }
  // tile.rs: EditorState::Date is now reachable for EditTarget::Cell; commit parses by kind:
  //   Number → parse_cell(ty); Text → trimmed verbatim (empty refused when `required`); Date → the field's value.
  ```

- [ ] **Step 1: Write the failing tests**

`matrix.rs`:

```rust
    /// §4.5: a cell commit patches instead of rebuilding, and the patch
    /// is exactly what a rebuild would have painted for that cell.
    #[test]
    fn patch_cell_matches_a_rebuild() {
        let snapshot = schedule_snapshot(&[("D1", "2026-12-18", 1.25, "declared"), ("D2", "2027-03-19", 0.5, "estimated")]);
        let mut draft = Draft::default();
        let mut patched = MatrixModel::build(&snapshot, &SCHEDULE, &draft).unwrap();
        draft.set((1, 1), ("D2".into(), "amount".into()), Value::F64(0.75), "t0");
        assert!(patched.patch_cell(1, 1, &snapshot, &SCHEDULE, &draft));
        let rebuilt = MatrixModel::build(&snapshot, &SCHEDULE, &draft).unwrap();
        assert_eq!(patched.rows[1].cells[1], rebuilt.rows[1].cells[1]);
        assert_eq!(patched.rows[0].cells[1], rebuilt.rows[0].cells[1], "untouched cells untouched");
        assert!(!patched.patch_cell(9, 0, &snapshot, &SCHEDULE, &draft), "out of range answers false");
    }
```

(`Cell` needs `PartialEq` — derive it; `SharedString` and `Value` both implement it.)

`tile.rs` window tests (copy the shape of `a_double_click_opens_the_editor_on_the_cell` and the `document_of` builder; add a `schedule(as_of)` snapshot builder mirroring `cvi(as_of)` over the `SCHEDULE`-shaped columns, and an `open_with` variant taking a spec — read `open_with`'s signature):

```rust
    /// §4.4: a Text cell commits its text verbatim; a Date cell opens the
    /// segmented date field in the cell; both land in the draft as typed
    /// values and paint by the column's kind.
    #[gpui::test]
    fn a_text_cell_commits_verbatim_and_a_date_cell_opens_the_date_field(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_flat(cx);            // a SCHEDULE panel with one delivered document
        // status is column 2 in the model: move there and type
        h.tile.update(&mut vcx, |t, cx| { t.cursor_to(0, Some(2), cx); });
        vcx.simulate_keystrokes("i");
        vcx.run_until_parked();
        assert!(h.tile.read_with(&vcx, |t, _| t.editor_state().is_some()));
        set_editor_text(&h, &mut vcx, "paid");
        vcx.simulate_keystrokes("enter");
        vcx.run_until_parked();
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.model().rows[0].cells[2].text.to_string()), "paid");
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.draft().edits.get(&(0, 2)).cloned()), Some(Value::Utf8("paid".into())));
        // ex date is column 0: `i` opens the date field, not a text input
        h.tile.update(&mut vcx, |t, cx| { t.cursor_to(0, Some(0), cx); });
        vcx.simulate_keystrokes("i");
        vcx.run_until_parked();
        assert!(h.tile.read_with(&vcx, |t, _| t.date_field().is_some()));
        assert!(h.tile.read_with(&vcx, |t, _| t.editor_state().is_none()));
        vcx.simulate_keystrokes("up enter");   // one day later, commit
        vcx.run_until_parked();
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.model().rows[0].cells[0].text.to_string()), "2026-12-19");
    }

    /// An empty commit on a required Text cell is refused with the editor open.
    #[gpui::test]
    fn an_empty_required_text_commit_is_refused(cx: &mut gpui::TestAppContext) { /* set_value(""), enter → notice "a value is required", editor still open */ }
```

Two helpers for the test module, beside `open_with`:

```rust
    /// Write `text` into the open cell editor the way a keystroke would land
    /// it — through the `InputState`, inside a window update.
    fn set_editor_text(h: &Harness, vcx: &mut gpui::VisualTestContext, text: &str) {
        let state = h.tile.read_with(vcx, |t, _| t.editor_state().expect("an editor is open"));
        vcx.update(|window, cx| state.update(cx, |s, cx| s.set_value(text, window, cx)));
    }

    /// The painted text of model cell `(row, col)`.
    fn cell_text(h: &Harness, vcx: &gpui::VisualTestContext, row: usize, col: usize) -> String {
        h.tile.read_with(vcx, |t, _| t.model().rows[row].cells[col].text.to_string())
    }
```

`open_flat(cx)` is `open_with` over the `SCHEDULE` spec (add a `pub(crate) const SCHEDULE: PanelSpec` in `core/spec.rs` behind `#[cfg(test)]`, the same one Task 3's matrix tests define — move it there so both test modules share it) and one delivered `schedule(as_of)` snapshot with rows `D1`/`D2`.

- [ ] **Step 2: Run to verify they fail** — the text commit is refused with "not a numeric cell" (Task 3's stub); the date cell opens a text input.

- [ ] **Step 3: Implement**

`matrix.rs::patch_cell`: locate the snapshot row for `row` (for a document row, `row` IS the snapshot row index under `flatten`; under a pivot use `Grid`'s row/col map — store `row_of`/`col_of` lookups on the model as `pivot_index: Option<PivotIndex>` built by `pivot` so a patch can find the snapshot row for `(row, col)`), then recompute the cell exactly as `cell_of` does and assign it. Answer `false` when out of range.

`tile.rs`:
- `begin_edit`'s `Cursor::Cell` arm reads `self.model.kind_of(col)`: `Date` → open `EditorState::Date` seeded from the cell's text (today's date if it does not parse), target `EditTarget::Cell`; `Number`/`Text` → the text `Input` as today; `Choice` → Task 5 (until then treat as `Text`).
- `commit_edit`'s `(EditorState::Date, EditTarget::Cell)` arm (today "unreachable") runs `field.complete_pending()` exactly as the attr arm, then `commit_cell_value(cell, labels, Value::Date(field.value()))`.
- Split `commit_cell_edit` into `commit_cell_edit(text)` (parse by kind: `Number` → `parse_cell(text, ty)` → `Value::F64`/`I64` by the column's declared type; `Text`/`Choice` → trimmed string, empty refused with `"a value is required"` when the `ValueColumn.required` is true, else allowed) and `commit_cell_value(cell, labels, value)` (the label-identity check, `draft.set`, `close_editor`, then `patch_cell` instead of `rebuild_model` — call `install_model` after the patch so the table refreshes; if `patch_cell` answers `false` fall back to `rebuild_model`).
- `nudge`'s cell arm: `Date` cells step the date field (the attr arm already does this — share it).

`delegate.rs`: the editor slot paints `EditorPaint::Date` in a cell when the tile's editor is a date field on that cell — extend the delegate's `editor` field to the `EditorPaint`-shaped enum the header already uses (read `header::render_date_field` and call it from `render_td` with the same arguments; the `MarketDataTile` entity the delegate needs for key routing is what `sync_cursor`/`install_model` can hand it — read how the delegate gets `editor` today and extend that path).

- [ ] **Step 4: Run** — `cargo test -p geode-marketdata 2>&1 | tail -3`; clippy on the crate.

- [ ] **Step 5: Harness + commit**

Entries: `matrix: patch_cell re-prepares the cell` (mutate `patch_cell`'s assignment away → `patch_cell_matches_a_rebuild`), `tile: a text cell commits verbatim` (mutate the `Text` arm to refuse → the window test). Commit: `"marketdata: text and date cells edit by kind; a commit patches one cell (spec §4.4–§4.5)"`.

---

### Task 5: The `Choice` cell — popup and stepping

**Files:**
- Modify: `crates/geode-marketdata/src/tile.rs` (`Popup::Choice`, `begin_edit`'s `Choice` arm, `step`/`step_back` verbs, `key_context`, `close_popup_with_window`, `holds_focus`)
- Modify: `crates/geode-marketdata/src/popup.rs` (`ChoicePopup` state + `render_choice`)
- Modify: `crates/geode-marketdata/src/content.rs` (`ACTIONS` + `DEFAULT_KEYMAP`: `"space" = "marketdata::step"`, `"shift+space" = "marketdata::step_back"` in normal mode; the fragment mirror test)
- Test: `tile.rs` window tests

**Interfaces:**
- Consumes: Phase 1's `geode_shell::choice::{ChoiceList, ChoiceKey, route, DEFAULT_CAP}`; `dialog::choice_rows` is NOT used (it is `pub(crate)` to `geode-shell`) — the popup paints its own rows in `popup.rs`'s picker style.
- Produces:
  ```rust
  pub(crate) struct ChoicePopup { pub input: Entity<InputState>, pub list: ChoiceList, cell: (usize, usize), labels: (SharedString, SharedString) }
  pub(crate) enum Popup { Menu(MenuState), Picker(PickerState), Choice(ChoicePopup) }
  // verbs: "step" | "step_back" — a Choice cell steps to the next/previous option (wrapping) through
  // commit_cell_value; any other kind answers "not a choice cell"; refused while Behind / empty / in the strip.
  ```

- [ ] **Step 1: Write the failing tests**

```rust
    /// §4.4: `space`/`shift+space` step a choice cell in place, wrapping; on any other cell they say so.
    #[gpui::test]
    fn space_steps_a_choice_cell_and_refuses_elsewhere(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_flat(cx);
        h.tile.update(&mut vcx, |t, cx| { t.cursor_to(0, Some(2), cx); }); // status = declared
        vcx.simulate_keystrokes("space");
        vcx.run_until_parked();
        assert_eq!(cell_text(&h, &vcx, 0, 2), "paid");
        vcx.simulate_keystrokes("shift-space shift-space");
        vcx.run_until_parked();
        assert_eq!(cell_text(&h, &vcx, 0, 2), "estimated", "wraps backward from declared");
        h.tile.update(&mut vcx, |t, cx| { t.cursor_to(0, Some(1), cx); }); // amount
        vcx.simulate_keystrokes("space");
        vcx.run_until_parked();
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.notice().map(str::to_string)), Some("not a choice cell".into()));
    }

    /// `i` on a choice cell opens the typeahead popup with the field focused; `enter` picks the lit
    /// option and commits it; `escape` closes with nothing written; a click elsewhere closes it.
    #[gpui::test]
    fn i_on_a_choice_cell_opens_a_typeahead_and_enter_picks(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_flat(cx);
        h.tile.update(&mut vcx, |t, cx| { t.cursor_to(0, Some(2), cx); });
        vcx.simulate_keystrokes("i");
        vcx.run_until_parked();
        assert!(h.tile.read_with(&vcx, |t, _| t.choice_popup_open()));
        assert!(h.tile.read_with(&vcx, |t, _| t.key_context().to_string().contains("insert")));
        vcx.simulate_input("can");
        vcx.simulate_keystrokes("enter");
        vcx.run_until_parked();
        assert!(!h.tile.read_with(&vcx, |t, _| t.choice_popup_open()));
        assert_eq!(cell_text(&h, &vcx, 0, 2), "cancelled");
        // focus is back on nothing of the tile's (blur-then-drop)
        assert!(!vcx.update(|window, cx| h.tile.read(cx).holds_focus(window, cx)));
    }
```

- [ ] **Step 2: Run to verify they fail** — `step` is an unknown verb; `i` opens a text input.

- [ ] **Step 3: Implement**

`popup.rs`: `ChoicePopup` (the picker's shape: an `InputState` that holds the keyboard plus a pure `ChoiceList` over the column's `choices`, placed on the cell's current text at open) and `render_choice(p, theme, tile, tile_id)` painting `list.painted()` rows in the picker's row geometry (`popover_style`, 26 px rows, `accent` highlight), anchored under the cursor cell — read `render_picker` and copy its anchoring; a row's mouse-down picks it (`choice_pick(row)`); `on_mouse_down_out` closes. `tile.rs`: `Popup::Choice`; `begin_edit`'s `Choice(options)` arm opens it (refusals as the text editor's) and focuses the field; `key_context` reports `mode == insert` while open; `holds_focus` answers its field; `close_popup_with_window` blurs its field when focused; `commit` (`enter`) re-feeds the live text through `list.set_query` then `pick()` → `commit_cell_value(cell, labels, Value::Utf8(option))` — refused with `"no option matches"` when nothing is lit; `insert_up`/`insert_down` move the highlight (the picker's own arms); `escape`/`cancel` closes. `dispatch`: `"step" | "step_back"` → `step_choice(delta)`: reads `kind_of(col)`, `Choice(options)` → current text's index in `options` (0 if absent) ± 1 wrapping → `commit_cell_value`, refused with `"not a choice cell"` otherwise, and with `BEHIND_REFUSED`/`NO_DOCUMENT`/strip refusals as `bump` uses. `content.rs`: two `ACTIONS` rows (`"marketdata::step"`, "Step value"; `"marketdata::step_back"`, "Step value back") and the two bindings in the normal-mode block; the mirror test's expected count grows by two. Add `pub(crate) fn choice_popup_open(&self) -> bool` for tests.

- [ ] **Step 4: Run** — `cargo test -p geode-marketdata 2>&1 | tail -3`; `cargo test -p geode-shell` (fragment checks read every module's keymap — `check_fragment` must still accept the block).

- [ ] **Step 5: Harness + commit** — entries `tile: space steps a choice cell` (mutate the wrap arithmetic to never advance) and `tile: enter in the choice popup picks the lit option` (mutate `pick()` to `Some(0)`). Commit `"marketdata: a Choice cell steps in place and opens a typeahead popup (spec §4.4)"`.

---

### Task 6: `Draft` row edits — `RowEdit`, rebase, session

**Files:**
- Modify: `crates/geode-marketdata/src/core/draft.rs`
- Test: `draft.rs` tests

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, PartialEq)]
  pub enum RowEdit {
      Inserted { after: Option<String>, cells: BTreeMap<String, Value> },   // cells keyed by COLUMN LABEL
      Deleted,
  }
  pub struct Draft { ..., pub rows: BTreeMap<String, RowEdit>, ... }
  impl Draft {
      pub fn insert_row(&mut self, label: String, after: Option<String>, base: &str);
      /// Dropped(true) = an Inserted row removed outright; Marked = a document row now Deleted; Already = it was Deleted.
      pub fn delete_row(&mut self, label: &str, base: &str) -> RowDelete;   // enum RowDelete { Dropped, Marked, Already }
      pub fn set_row_cell(&mut self, label: &str, column_label: &str, value: Value) -> bool;  // false unless the row is Inserted
      pub fn rename_row(&mut self, from: &str, to: &str) -> bool;   // Inserted rows only; false if `to` exists in `rows`
      pub fn row_state(&self, label: &str) -> Option<&RowEdit>;
      pub fn rows_added(&self) -> usize; pub fn rows_removed(&self) -> usize;
      pub fn mint_label(&self, taken: impl Fn(&str) -> bool) -> String;  // "new-<n>", smallest n with !taken && not in rows
      pub fn len(&self) -> usize   // cells + attrs + rows
      pub fn count_phrase(&self) -> String   // "2 cells, 1 row added, 1 row removed, spot_ref"
      pub fn revert(&mut self) -> usize      // clears rows too
      pub fn rebase(&mut self, model_of_newer: &MatrixModel) -> (usize, Vec<(String, String)>)  // rows per §5.1; a dropped row is reported as (label, "row")
  }
  ```
  `to_toml`: `[rows.<label>]` with `after = "<label>"` (omitted for top) and `cells = { <column label> = <typed value as Task 3 spells a cell> }`, or `deleted = true`. `from_toml` reads it back. `is_empty()`/`badge()` count rows.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn inserting_and_deleting_rows_is_counted_and_phrased() {
        let mut d = Draft::default();
        d.insert_row("new-1".into(), Some("D1".into()), "t0");
        assert!(d.set_row_cell("new-1", "amount", Value::F64(1.0)));
        assert!(!d.set_row_cell("D1", "amount", Value::F64(1.0)), "not an inserted row");
        assert_eq!(d.delete_row("D2", "t0"), RowDelete::Marked);
        assert_eq!(d.delete_row("D2", "t0"), RowDelete::Already);
        assert_eq!(d.rows_added(), 1);
        assert_eq!(d.rows_removed(), 1);
        assert_eq!(d.count_phrase(), "1 row added, 1 row removed");
        assert_eq!(d.delete_row("new-1", "t0"), RowDelete::Dropped);
        assert_eq!(d.rows_added(), 0);
        assert_eq!(d.revert(), 1);
        assert!(d.rows.is_empty());
    }

    #[test]
    fn mint_label_takes_the_smallest_unused_number() {
        let mut d = Draft::default();
        assert_eq!(d.mint_label(|_| false), "new-1");
        d.insert_row("new-1".into(), None, "t0");
        assert_eq!(d.mint_label(|_| false), "new-2");
        assert_eq!(d.mint_label(|l| l == "new-2"), "new-3", "a label the model already has is skipped");
    }

    #[test]
    fn rename_row_moves_an_inserted_row_and_refuses_a_collision() {
        let mut d = Draft::default();
        d.insert_row("new-1".into(), None, "t0");
        d.insert_row("new-2".into(), None, "t0");
        assert!(d.rename_row("new-1", "2027-01-15"));
        assert!(d.row_state("2027-01-15").is_some());
        assert!(!d.rename_row("new-2", "2027-01-15"));
        assert!(!d.rename_row("D1", "x"), "only an inserted row renames");
    }

    /// §5.1: rebase carries rows by label — a deleted row whose label vanished is dropped and named,
    /// an inserted row whose label the newer document now carries is dropped and named, a vanished
    /// anchor re-anchors to the top and is named.
    #[test]
    fn rebase_carries_rows_by_label() {
        let mut d = Draft::default();
        d.delete_row("GONE", "t0");
        d.delete_row("D1", "t0");
        d.insert_row("D9".into(), Some("D1".into()), "t0");       // upstream will carry D9
        d.insert_row("new-1".into(), Some("GONE".into()), "t0");  // anchor vanishes
        let newer = flat_model_with_rows(&["D1", "D2", "D9"]);
        let (_, dropped) = d.rebase(&newer);
        assert!(matches!(d.row_state("D1"), Some(RowEdit::Deleted)));
        assert!(d.row_state("GONE").is_none());
        assert!(d.row_state("D9").is_none(), "upstream got there first");
        assert!(matches!(d.row_state("new-1"), Some(RowEdit::Inserted { after: None, .. })));
        assert!(dropped.contains(&("GONE".into(), "row".into())));
        assert!(dropped.contains(&("D9".into(), "row (the document now carries it)".into())));
        assert!(dropped.contains(&("new-1".into(), "anchor 'GONE'".into())));
    }

    #[test]
    fn rows_round_trip_through_toml() {
        let mut d = Draft::default();
        d.insert_row("new-1".into(), Some("D1".into()), "t0");
        d.set_row_cell("new-1", "ex", Value::Date(date(2027, 1, 15)));
        d.set_row_cell("new-1", "status", Value::Utf8("estimated".into()));
        d.delete_row("D2", "t0");
        let back = Draft::from_toml(&d.to_toml());
        assert_eq!(back.rows, d.rows);
        assert_eq!(back.base, d.base);
    }
```

- [ ] **Step 2: Run to verify they fail** — compile errors.

- [ ] **Step 3: Implement** per the interface. `insert_row`/`delete_row` set `base` when it is `None` exactly as `set` does and move `state` to `Editing`. `count_phrase` order: cells, rows added, rows removed, then attribute names. `rebase` for rows: build the newer model's label set once; for each `(label, edit)`: `Deleted` → keep if present else drop `(label, "row")`; `Inserted { after, .. }` → if present drop `(label, "row (the document now carries it)")`, else keep, and if `after` is `Some(a)` with `a` absent, set `after = None` and push `(label, format!("anchor '{a}'"))`. `to_toml`/`from_toml` per the interface (cells reuse Task 3's `value_to_toml`/`value_from_toml`).

- [ ] **Step 4: Run** — `cargo test -p geode-marketdata draft 2>&1 | tail -3`.

- [ ] **Step 5: Harness + commit** — entries `draft: an inserted row the document now carries is dropped on rebase` (mutate the presence check) and `draft: mint_label never reuses a label` (mutate the `rows` lookup out). Commit `"marketdata: Draft carries row insert/delete by label (spec §5.1, §5.4)"`.

---

### Task 7: Rows in the model — splice, `RowState`, painting, incomplete count

**Files:**
- Modify: `crates/geode-marketdata/src/core/matrix.rs` (`RowState`, `RowModel.state`, `build` splices/marks; `patch_cell` handles inserted rows by rebuilding their cells from `RowEdit.cells`)
- Modify: `crates/geode-marketdata/src/core/draft.rs` (`incomplete_rows(&self, spec) -> usize`)
- Modify: `crates/geode-marketdata/src/delegate.rs` (`cell_paint` gains the row state; a `Deleted` row paints `line_through` in `muted_foreground`, an `Inserted` row's cells a `success` 18% tint under `foreground`; the row-label cell follows the same)
- Modify: `crates/geode-marketdata/src/header.rs` (`HeaderModel::prepare`: the dirty dot counts rows; a `N rows incomplete` chip in `Tone::Warn` when `incomplete_rows > 0`)
- Modify: `crates/geode-marketdata/src/tile.rs` (readers for tests: `row_state_at(row)`)
- Test: `matrix.rs`, `delegate.rs` (theme sweep), `header.rs`

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum RowState { Document, Inserted, Deleted }
  pub struct RowModel { pub label: SharedString, pub cells: Vec<Cell>, pub state: RowState }
  // build: document rows in document order, each Deleted-marked where draft.rows says so; then each Inserted row
  // spliced AFTER its anchor's current position (top for None; several under one anchor in label order), its
  // cells from RowEdit.cells by column label (missing → text "·", value None), edited: true.
  // A model row's cell_ref for an inserted row is (row index, col); Draft::edits never holds one.
  impl Draft { pub fn incomplete_rows(&self, spec: &PanelSpec) -> usize }  // Inserted rows missing a required column
  //   required = every ValueColumn with `required` under Values; EVERY column (ladder + slice values) under Axis.
  pub(crate) fn cell_paint(theme: &Theme, sent: bool, edited: bool, state: RowState) -> CellPaint
  ```

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn inserted_rows_splice_after_their_anchor_and_deleted_rows_stay_marked() {
        let snapshot = schedule_snapshot(&[("D1", ..), ("D2", ..), ("D3", ..)]);
        let mut d = Draft::default();
        d.insert_row("new-1".into(), Some("D1".into()), "t0");
        d.insert_row("new-2".into(), None, "t0");
        d.set_row_cell("new-1", "amount", Value::F64(2.0));
        d.delete_row("D3", "t0");
        let m = MatrixModel::build(&snapshot, &SCHEDULE, &d).unwrap();
        let labels: Vec<_> = m.rows.iter().map(|r| r.label.to_string()).collect();
        assert_eq!(labels, ["new-2", "D1", "new-1", "D2", "D3"]);
        assert_eq!(m.rows[0].state, RowState::Inserted);
        assert_eq!(m.rows[2].cells[1].text.as_ref(), "2.0000");
        assert_eq!(m.rows[2].cells[0].text.as_ref(), "·", "an unfilled cell paints a dot");
        assert_eq!(m.rows[4].state, RowState::Deleted);
        assert_eq!(d.incomplete_rows(&SCHEDULE), 2, "new-1 lacks ex and status; new-2 lacks all three");
    }
    #[test]
    fn a_pivot_inserted_row_is_incomplete_until_every_cell_is_filled() { /* CVI: insert "2027-01-15", fill 12 nodes + fwd/atm/skew → 0 */ }
```

`delegate.rs`: extend the existing `dirty_and_sent_cells_are_readable_on_every_bundled_theme` sweep with the `Inserted` fill (foreground over `success` at 0.18 over the table ground, ≥ 3:1 on every theme — if any theme fails, floor through `readable_on` as the header tones do and say so in the report) and the `Deleted` text (`muted_foreground` over the ground — this is the theme's own pairing, unfloored, as CLAUDE.md records for `Plain`; assert nothing beyond "the same colour `Plain` uses").

- [ ] **Step 2: Run to verify they fail**.

- [ ] **Step 3: Implement** per the interface. In `build`, after `pivot`/`flatten` produce document rows, run `splice_rows(rows, draft, &column_kinds, &columns)`: mark deleted by label; collect inserted rows grouped by anchor; walk the document rows emitting each and then its inserted followers (label-sorted); `None`-anchored rows first. `patch_cell` on an `Inserted` row recomputes that cell from `RowEdit.cells`. `cell_paint`'s `state` arm: `Deleted` → `fill: None, text: theme.muted_foreground, strike: true` (add `strike: bool` to `CellPaint`; `render_td` applies `line_through()`); `Inserted` → `fill: Some(theme.success.opacity(0.18))`, text floored as the sweep demands. Header: `prepare` reads `draft.rows_added()/rows_removed()` into the dirty state and paints the incomplete chip.

- [ ] **Step 4: Run** — crate tests + the theme sweeps.

- [ ] **Step 5: Harness + commit** — entries `matrix: an inserted row lands after its anchor` (mutate the splice to append at the end) and `matrix: a deleted row stays painted` (mutate the mark into a removal). Commit `"marketdata: inserted and deleted rows in the model, painted and counted (spec §5.2)"`.

---

### Task 8: Row verbs — `o`, `shift+o`, `d d`, the row-label editor, session

**Files:**
- Modify: `crates/geode-marketdata/src/content.rs` (`ACTIONS` + `DEFAULT_KEYMAP`: `"o" = "marketdata::insert_below"`, `"shift+o" = "marketdata::insert_above"`, `"d d" = "marketdata::delete_row"`)
- Modify: `crates/geode-marketdata/src/tile.rs` (the three verbs; `EditTarget::RowLabel { row, label }`; `commit_edit`'s label arm; `serialize`/restore carry `rows` through `Draft::to_toml` — already does, since parking and session write the whole table; `yank_row` on an inserted row)
- Modify: `crates/geode-marketdata/src/delegate.rs` (`label_editor: Option<(usize, Entity<InputState>)>` painted in column 0)
- Test: `tile.rs` window tests

**Interfaces:**
- Consumes: Task 6's `Draft::{insert_row, delete_row, mint_label, rename_row, set_row_cell}`, Task 7's `RowState`.
- Produces: the three verbs; `EditTarget::RowLabel`; `pub(crate) fn label_editor_state(&self) -> Option<Entity<InputState>>` for tests.
- **Deviation from spec §5.3, ruled here:** the row-label editor for a `Typed` axis is the text `Input` for every axis type — a `Date` axis is typed as `YYYY-MM-DD` and parsed through `parse_attr(text, ty)` — not the segmented date field. The delegate's label column would otherwise need the segmented field's key routing wired into the table cell; the spec's As-built note records this (Task 13).

- [ ] **Step 1: Write the failing tests**

```rust
    /// §5.3 on a Minted axis: `o` inserts `new-1` below the cursor row and lands the cursor on its
    /// first cell in insert mode; `shift+o` inserts above; `d d` on the inserted row drops it, on a
    /// document row marks it Deleted (and says so a second time); all three are refused while Behind.
    #[gpui::test]
    fn o_inserts_a_minted_row_and_dd_deletes(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_flat(cx);   // rows D1, D2
        vcx.simulate_keystrokes("o");
        vcx.run_until_parked();
        assert_eq!(row_labels(&h, &vcx), ["D1", "new-1", "D2"]);
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.cursor()), Cursor::Cell { row: 1, col: 0 });
        assert!(h.tile.read_with(&vcx, |t, _| t.date_field().is_some()), "the first cell (ex, a Date) opened its editor");
        vcx.simulate_keystrokes("escape");
        vcx.simulate_keystrokes("shift-o");
        vcx.run_until_parked();
        assert_eq!(row_labels(&h, &vcx), ["D1", "new-2", "new-1", "D2"]);
        vcx.simulate_keystrokes("escape d d");
        vcx.run_until_parked();
        assert_eq!(row_labels(&h, &vcx), ["D1", "new-1", "D2"], "an inserted row is dropped outright");
        h.tile.update(&mut vcx, |t, cx| { t.cursor_to(2, Some(0), cx); });
        vcx.simulate_keystrokes("d d");
        vcx.run_until_parked();
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.row_state_at(2)), Some(RowState::Deleted));
        vcx.simulate_keystrokes("d d");
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.notice().map(str::to_string)), Some("row is already deleted — :revert restores it".into()));
        assert!(h.tile.read_with(&vcx, |t, _| t.header_dirty()));
    }

    /// §5.3 on a Typed axis (CVI): `o` inserts a provisional row and opens the row-label editor on it;
    /// `enter` with a new term renames the row; a term already present is refused with the editor
    /// open; `escape` drops the provisional row.
    #[gpui::test]
    fn o_on_a_typed_axis_opens_the_label_editor(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);   // the existing CVI fixture, terms include 2026-10-16
        vcx.simulate_keystrokes("o");
        vcx.run_until_parked();
        assert!(h.tile.read_with(&vcx, |t, _| t.label_editor_state().is_some()));
        set_label_editor_text(&h, &mut vcx, "2026-10-16"); // an existing term
        vcx.simulate_keystrokes("enter");
        vcx.run_until_parked();
        assert!(h.tile.read_with(&vcx, |t, _| t.label_editor_state().is_some()), "refused, still open");
        assert!(h.tile.read_with(&vcx, |t, _| t.notice().unwrap_or("").contains("already")));
        set_label_editor_text(&h, &mut vcx, "2027-01-15");
        vcx.simulate_keystrokes("enter");
        vcx.run_until_parked();
        assert!(row_labels(&h, &vcx).contains(&"2027-01-15".to_string()));
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.model().rows.iter().filter(|r| r.state == RowState::Inserted).count()), 1);
        vcx.simulate_keystrokes("o");
        vcx.run_until_parked();
        vcx.simulate_keystrokes("escape");
        vcx.run_until_parked();
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.draft().rows_added()), 1, "escape dropped the provisional row");
    }

    /// `set_editor_text`'s twin for the row-label editor (`label_editor_state`).
    fn set_label_editor_text(h: &Harness, vcx: &mut gpui::VisualTestContext, text: &str) {
        let state = h.tile.read_with(vcx, |t, _| t.label_editor_state().expect("the label editor is open"));
        vcx.update(|window, cx| state.update(cx, |s, cx| s.set_value(text, window, cx)));
    }

    /// The model's row labels in painted order.
    fn row_labels(h: &Harness, vcx: &gpui::VisualTestContext) -> Vec<String> {
        h.tile.read_with(vcx, |t, _| t.model().rows.iter().map(|r| r.label.to_string()).collect())
    }

    #[gpui::test]
    fn row_verbs_are_refused_while_behind_and_in_the_strip(cx: &mut gpui::TestAppContext) { /* Behind → BEHIND_REFUSED; cursor in Attr → "not a row" */ }

    #[gpui::test]
    fn row_edits_ride_the_session_and_the_parked_map(cx: &mut gpui::TestAppContext) { /* o, fill a cell, serialize → [drafts.<key>.rows.new-1]; restore → rows_added 1 */ }
```

- [ ] **Step 2: Run to verify they fail**.

- [ ] **Step 3: Implement**

`insert_row_at(below: bool)`: refusals (editor open → cancel it first exactly as `set_key` does; `Behind`; empty model; strip); anchor = the cursor row's label for `below`, the previous row's label (or `None` at row 0) for `above`; `label = draft.mint_label(|l| model has a row labelled l)`; `draft.insert_row(label, anchor, base)`; `rebuild_model`; move the cursor to the new row (find by label), col 0; then for `RowIdentity::Minted` run `begin_edit` on the cell; for `Typed(ty)` open the label editor: `EditTarget::RowLabel { row, label }` with an `InputState` seeded empty, focused, mirrored to the delegate's `label_editor`. `commit_edit`'s `RowLabel` arm: parse through `parse_attr(text, ty)` → `attr_text` as the canonical label; refuse `"'{label}' is already a row"` if any model row has it; `draft.rename_row(old, new)`, close the editor, rebuild, cursor stays on the row, and then `begin_edit` on col 0 (the first cell to fill). `close_editor` on a `RowLabel` target whose row is still provisional (label starts with `new-` and no cell filled) drops the row through `delete_row` and rebuilds. `delete_row` verb: `d d` → `draft.delete_row(label)` → `Dropped` rebuild + clamp; `Marked` rebuild; `Already` notice. `yank_text(Row)` on an inserted row yanks `cells` as painted. `content.rs` gets the three actions and bindings.

- [ ] **Step 4: Run** — crate tests; `cargo test -p geode-shell` (fragment).

- [ ] **Step 5: Harness + commit** — entries: `tile: o inserts after the cursor row` (mutate the anchor to `None`), `tile: a duplicate typed label is refused` (mutate the collision check away), `tile: escape drops a provisional row` (mutate the drop). Commit `"marketdata: o / shift+o / d d — row insert and delete on both layouts (spec §5.3)"`.

---

### Task 9: `DividendKind`

**Files:**
- Create: `crates/geode-documents/src/dividend.rs`
- Modify: `crates/geode-documents/src/lib.rs` (`pub mod dividend; pub use dividend::DividendKind;` and `builtin_kinds()` returns both)
- Test: `dividend.rs` tests; `crates/geode-documents/benches/` gets a `dividend` parse/write bench beside CVI's (same harness)

**Interfaces:**
- Produces:
  ```rust
  pub const NAME: &str = "dividend_schedule";
  pub const STATUSES: [&str; 4] = ["estimated", "declared", "paid", "cancelled"];
  pub struct DividendKind;   // impl DocumentKind: name, columns, parse, write
  pub const COLUMNS: &[(&str, ColumnType)] = &[
      ("underlying_ref", Utf8), ("dividend_id", Utf8), ("ex_date", Date), ("announced_date", Date),
      ("pay_date", Date), ("amount", F64), ("status", Utf8), ("currency", Utf8), ("schedule_date", Date),
  ];
  ```
  Wire (assumption, one `TAGS` table): `<marketData><underlying>SPX</underlying><dividends><currency>USD</currency><scheduleDate>2026-09-19</scheduleDate><dividend><id>…</id><exDate>…</exDate><announcedDate>…</announcedDate><payDate>…</payDate><amount>1.25</amount><status>declared</status></dividend>…</dividends></marketData>`.
  Refusals (parse): missing/repeated `id`, an `id` beginning `new-`, unknown `status`, a malformed date/amount, a repeated at-most-once element, a `<dividend>` missing any of its six children — each names the element path. Unknown elements are skipped and logged once per (source, path) exactly as CVI does. `write` refuses a status outside `STATUSES` and a zero-row document.

- [ ] **Step 1: Write the failing tests** (mirror `cvi.rs`'s test module: `parses_a_well_formed_document`, `round_trips_through_write`, one test per refusal above with the message asserted to name the path, `unknown_elements_are_skipped`, `a_new_prefixed_id_is_refused`).

- [ ] **Step 2: Run to verify they fail** (module missing).

- [ ] **Step 3: Implement** — a `quick_xml::Reader` event walk in `cvi.rs`'s exact structure (`Node` classification enum, path stack, `already_filled`, `parse_err`), landing in `DocumentRows { key: [underlying], attributes: [(currency, Utf8), (schedule_date, Date)], axes: [(dividend_id, Utf8 column)], values: [ex_date Date, announced_date Date, pay_date Date, amount F64, status Utf8] }` in `COLUMNS` order; `write` the inverse through `quick_xml::Writer`.

- [ ] **Step 4: Run** — `cargo test -p geode-documents`, `cargo bench -p geode-documents --no-run`.

- [ ] **Step 5: Harness + commit** — entries: `dividend: a new- id is refused`, `dividend: an unknown status is refused`, `dividend: write refuses an unknown status`. Commit `"documents: DividendKind — parse and write the dividend schedule (spec §6.2)"`.

---

### Task 10: `DividendGenerator`

**Files:**
- Modify: `crates/geode-demo-data/src/documents.rs` (a `pub mod dividend` beside `cvi`)
- Test: same file; `crates/geode-demo-data/benches/` gets a `next_document` bench beside CVI's

**Interfaces:**
- Produces:
  ```rust
  pub struct DividendGenerator { /* seeded StdRng per key, per-key schedule state */ }
  impl DividendGenerator {
      pub fn new(seed: u64, underlyings: Vec<String>, today: NaiveDate) -> Self;
      pub fn underlyings(&self) -> &[String];
      pub fn next_document(&mut self, key: &str) -> DocumentRows;   // in DividendKind::COLUMNS order
  }
  ```
  Shape (§6.3): `SPX`/`NDX`/`RUT` get 30–40 rows with 2–3 same-ex-date pairs; other names 8–12 quarterly rows plus an occasional special; ids `D<fnv1a(key) % 100000>-<ordinal>` stable per (underlying, ordinal); `status` by ex date (past → `paid`, within 30 days → `declared`, else `estimated`; one in twenty `cancelled`); each republish walks one or two amounts (`MAX_WALK_STEP` 0.05, never below 0.01), promotes one `estimated` to `declared` when one is within 30 days, and every fifth republish appends a row; rows sorted by `(ex_date, id)`; `announced_date` = ex − 30..60 days, `pay_date` = ex + 14..28 days; `currency` `"USD"`, `schedule_date` = today.

- [ ] **Step 1: Write the failing tests** — `same_seed_same_documents` (two generators, same seed → identical first documents per key), `an_index_schedule_has_same_day_pairs` (SPX: at least two ex dates with >1 row), `ids_are_stable_across_republishes` (ids of the first document ⊆ ids of the fifth), `a_republish_appends_by_the_fifth`, `rows_are_sorted_by_ex_date_then_id`, `every_status_is_in_the_closed_set` (against `geode_documents`? no — `geode-demo-data` must not depend on it; assert against a local copy of the four words and add a test in `geode-app` (Task 11) that the two agree).

- [ ] **Step 2–4**: implement, run `cargo test -p geode-demo-data`, `cargo bench -p geode-demo-data --no-run`.

- [ ] **Step 5: Commit** — `"demo-data: DividendGenerator — seeded dividend schedules per underlying (spec §6.3)"`.

---

### Task 11: The demo bus round-robins producers; `[dividend]` source and dataset

**Files:**
- Modify: `crates/geode-app/src/demo_bus.rs` (`Producer`, `spawn(feed, producers, cadence, jitter, seed)`, the loop)
- Modify: `crates/geode-app/src/main.rs` (build two producers)
- Modify: `crates/geode-app/src/demo.rs` (`[dividend]` source in `layer`; test)
- Modify: `examples/demo-config/datasets.toml` (`[dividend_schedule]`, the exact table in spec §6.1)
- Test: `demo_bus.rs` (the existing bus test with two producers; a test that the two vocabularies agree: `DividendKind::STATUSES` == the generator's), `demo.rs`

**Interfaces:**
- Produces:
  ```rust
  pub struct Producer {
      pub kind: Arc<dyn DocumentKind>,
      pub topic_prefix: &'static str,   // "marketdata/cvi/" | "marketdata/dividend/"
      pub keys: Vec<String>,
      pub next: Box<dyn FnMut(&str) -> DocumentRows + Send>,
  }
  pub fn spawn(feed: ChannelFeed, producers: Vec<Producer>, cadence: Duration, jitter: Duration, seed: u64) -> DemoBus;
  ```
  Schedule: every producer's every key once at start (producer order, key order); then one publish per `cadence ± jitter` walking a round-robin list `[(p0,k0), (p1,k0), (p0,k1), (p1,k1), …]` (zip-longest across producers).

- [ ] **Step 1: Write the failing tests** — extend `the_bus_publishes_every_key_once_at_start_then_on_its_cadence` to two producers (three CVI keys + two dividend keys → burst of five, then the first two cadence publishes are one of each prefix); `demo.rs`: `the_demo_layer_declares_the_dividend_source` (adapter `demo_bus`, dataset/document `dividend_schedule`, topics `["marketdata/dividend/>"]`); the demo schema test (`crates/geode-app/src/demo.rs` ~line 203) asserts `dividend_schedule` parses with no diagnostics and `check_kind_against(&DividendKind, ds)` is `Ok`.

- [ ] **Step 2–4**: implement; `main.rs` builds `vec![Producer{cvi…}, Producer{dividend…}]` over `demo_underlyings()`; run `cargo test -p geode-app`, then `cargo run -p geode-app -- --demo 1000` for ten seconds and confirm the log shows publishes on both prefixes (the demo database at `$TMPDIR/geode-demo/1000-42/` needs no deletion — a NEW dataset's tables are created on open).

- [ ] **Step 5: Harness + commit** — entry `demo bus: publishes round-robin across producers` (mutate the schedule to walk producer 0 only). Commit `"demo: the bus feeds two producers; [dividend] source and dividend_schedule dataset (spec §6.1, §6.4)"`.

---

### Task 12: `DIVIDEND` spec and the second factory

**Files:**
- Modify: `crates/geode-marketdata/src/core/spec.rs` (`pub const DIVIDEND: PanelSpec`, exactly spec §6.5, `choices: Some(&STATUSES_COPY)` — `geode-marketdata` must not depend on `geode-documents`; add `pub const STATUSES: [&str; 4]` here and a `geode-app` test asserting it equals `DividendKind::STATUSES`)
- Modify: `crates/geode-marketdata/src/content.rs` (`MarketDataFactory::without_keymap(self) -> Self`: `default_keymap()` answers `None` — one vocabulary, one fragment; `register_actions` is already tolerant)
- Modify: `crates/geode-app/src/bridge.rs` (`Bridge.dividend: Rc<MarketDataFactory>` beside `marketdata`, built `.without_keymap()`, `set_stale_after` on reload; the five test constructions)
- Modify: `crates/geode-app/src/main.rs` (`roster.add` the second handle)
- Test: `content.rs` (`without_keymap` ships no fragment, still registers), `bridge.rs`/`main.rs` (the palette lists `Dividend: Split`)

- [ ] **Step 1–5**: tests → implement → run `cargo test -p geode-app -p geode-marketdata` → harness entry `factory: the second panel ships no second fragment` (mutate `without_keymap` to keep the keymap; the test asserts `keymap_fragments()` yields one `<module:cvi>` doc and no `<module:dividend>`) → commit `"marketdata: DIVIDEND panel spec; the app registers it as a second factory (spec §6.5)"`.

---

### Task 13: Benches, perf numbers, docs

**Files:**
- Modify: `crates/geode-marketdata/benches/matrix.rs` (`patch_cell` at 20×30 and 10,000×5; `build` 10,000×5 with 100 inserted rows; `rebase` with 1,000 cells + 100 rows)
- Modify: `docs/perf.md` ("Market-data panel" section: the new numbers, and the note that the flat build is no longer a per-commit cost)
- Modify: `docs/phase-history.md` (one paragraph at the end: the dividend slice, in the CLAUDE.md house voice — what it built, the rulings, the traps: the label-editor deviation, `flex_shrink`/`patch_cell` rules, `click_opened_stage`-style guards if any, the `new-` prefix, the second factory's `without_keymap`)
- Modify: `CLAUDE.md` (a status row `Dividend schedule (2026-09-19)`; rule bullets under "Market-data panel": typed cells + `patch_cell`; rows by label, `new-` minted, deleted rows stay painted; the two-producer bus; `without_keymap`)
- Modify: the choice/dividend spec (§4–§6 "As built" note incl. the label-editor deviation and anything else the tasks deviated on), the market-data documents spec (§3/§6 value types, §8.1 `Columns::Values` shape — the amendments §7 lists), the roadmap's §6 status line ("dividends: done 2026-09-19")

- [ ] **Step 1**: run `cargo bench -p geode-marketdata` and record p50s.
- [ ] **Step 2**: write the docs; verify every sentence against the code (the field-help ruling: help copy is a behaviour claim).
- [ ] **Step 3**: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo bench --workspace --no-run && zsh scripts/mutation-check.sh --anchors-only`.
- [ ] **Step 4**: commit `"docs: dividend schedule — perf numbers, phase history, CLAUDE.md, spec as-built"`.
