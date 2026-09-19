# Scopes Dialog Editing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The Scopes dialog edits a saved scope's own criteria — its dimension selections (ticked from the data's distinct values), its text filter and its expression — and creates an empty scope with `n` or a copy with `c`, instead of only snapshotting the frame.

**Architecture:** `Domain::Scopes` gains three real fields (`dimensions` as an `OrderedList` with an available block of `pickable_columns`, `text` and `expression` as `i`-editable `Text`s). A new `Stage::Values { object, column }` is a projection over the same `Draft` in `Stage::Column`'s mould: it stashes the scope's fields and installs ONE `OrderedList` whose items are the column's distinct values with `included` as the tick (Groupings' "ticking is membership" shape — see the spec amendment in Task 1), fetched through `ShellEvent::DistinctRequested` under a new reserved `SCOPES_KEY` and routed by `ShellView::deliver_distinct` on `outcome.key`. Every mutation folds into `draft.source` (`scopes::fold`), so `to_table` keeps rendering `source` verbatim and `Draft::is_dirty`, the write path, the fork rule and the diagnostics glyphs are untouched.

**Tech Stack:** Rust, gpui / gpui-component 0.6.2 (pinned), `geode_core::scopes::{saved_scopes_from_doc, scope_to_table}`, `geode_core::scope::parse_expr`, `geode_shell::shell::pickable_columns`, `TestAppContext` window tests, `scripts/mutation-check.sh`.

**Spec:** `docs/superpowers/specs/2026-09-19-geode-scopes-dialog-editing-design.md` (amended in Task 1, §4).

## Global Constraints

- Dimension values are never typed: they come from a `DistinctOutcome` (spec ruling 1). A saved value the outcome lacks paints ticked with the note `not in data` and is dropped only by an untick (ruling 5).
- Reordering a scope's selections is refused with the notice `selections have no order` (ruling 4); the footer names no reorder group on Scopes.
- A broken expression never reaches disk: `Domain::Scopes.parse_text("expression", ..)` refuses with `parse_expr`'s own message (spec §5).
- An emptied selection is never written as an empty array — `source.dimensions.<col>` is removed (spec §4, `scope_to_table`'s own rule).
- A transition site only mutates `mode`/`query`/`stage`/draft state; `dialog::sync_dialog_text` is the only thing that focuses or writes the shared `Input` (CLAUDE.md, "Dialogs now have two interaction modes").
- A stage door derives from `apply::config_with_pending`, never `services.config` alone (CLAUDE.md, §18.8's Major).
- `Draft::to_table` for Scopes renders `draft.source` and nothing else; every edit folds into `source` first (spec §3).
- The pure core (`objectdialog/mod.rs`, `objectdialog/scopes.rs`) imports no `gpui`.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo check -p geode-shell --features test-support --all-targets` and `zsh scripts/mutation-check.sh --anchors-only` must all pass before merge. Both macOS and Windows build in CI.
- Commit after every task with the attribution line `Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>`.

---

### Task 1: Scaffold groundwork — `ListItem.note`, `Draft.values`, the spec amendment

**Files:**
- Modify: `docs/superpowers/specs/2026-09-19-geode-scopes-dialog-editing-design.md` (§4)
- Modify: `crates/geode-shell/src/shell/objectdialog/mod.rs` (`ListItem`, `Stage`, `Draft`, `Draft::step_selected`, `ObjectDialogState::has_previous_stage`, every `ListItem { .. }` literal)
- Modify: `crates/geode-shell/src/shell/objectdialog/views.rs`, `groupings.rs`, `dataset_columns.rs` (every `ListItem { .. }` literal gains `note: None`)
- Test: `crates/geode-shell/src/shell/objectdialog/mod.rs` (`mod tests`)

**Interfaces:**
- Produces (used by Tasks 2–5):
  ```rust
  pub struct ListItem { /* existing */ pub note: Option<String> }   // muted text after the name; None = Views' column_summary as today
  pub enum Stage { /* existing */ Values { object: String, column: String } }
  impl Draft {
      pub fn values(&self) -> Option<&str>;                              // the open Values stage's column
      pub fn enter_values(&mut self, column: &str, fields: Vec<Field>) -> bool;
      pub fn leave_values(&mut self) -> Option<Vec<Field>>;             // restores parent fields; returns the stage's own fields for the caller's fold
  }
  ```

- [ ] **Step 1: Amend the spec's §4**

Replace the paragraph beginning "A projection over the same `Draft`, exactly as `Stage::Column` is:" up to "Crumb `<object> › <column>`, pill as the edit stage's." with:

```markdown
A projection over the same `Draft`, exactly as `Stage::Column` is:
`enter_values_stage` stashes the scope's fields as `parent_fields` and
installs ONE field, `values: FieldKind::OrderedList { items, available:
None }` — Groupings' "ticking IS membership" shape, each distinct value
one `ListItem` whose `included` is the tick and whose new `note`
carries the count (or `not in data`). **Amendment (plan, 2026-09-19):**
the brainstorm named `MultiChoice` plus a fourth `EditRow::Option`
variant here; the plan uses the list shape instead, because every
consumer (`rows`, `row_label`, `visible_rows`, the tick click, the
filter, the `ctrl+a`/`ctrl+x` walk) already exists for it and
`MultiChoice` has never had a row model. `Draft.values: Option<String>`
sits beside `Draft.column` so `revalidate` can fold the stage and
`step_selected`'s "keep at least one entry" guard can stand down there.
Crumb `<object> › <column>`, pill as the edit stage's.
```

- [ ] **Step 2: Write the failing pure tests**

Append to `mod tests` in `crates/geode-shell/src/shell/objectdialog/mod.rs`:

```rust
    /// The Values stage is a projection like the column stage: entering
    /// swaps the fields, leaving restores them and hands the stage's own
    /// fields back so the adapter can fold them.
    #[test]
    fn entering_values_swaps_the_fields_and_leaving_restores_them() {
        let mut draft = groupings_draft();
        let before = draft.fields.clone();
        let values = vec![Field {
            key: "values".to_string(),
            label: "Values".to_string(),
            kind: FieldKind::OrderedList {
                items: vec![item("BK001")],
                available: None,
            },
            dest: Destination::Doc,
            layer: None,
        }];
        assert!(draft.enter_values("book", values.clone()));
        assert_eq!(draft.values(), Some("book"));
        assert_eq!(draft.fields, values);
        assert!(!draft.is_dirty(), "freshly installed values are not dirt");
        // Re-entry is refused, as `enter_column` refuses it.
        assert!(!draft.enter_values("lhu", Vec::new()));
        let own = draft.leave_values().expect("the stage's fields");
        assert_eq!(own, values);
        assert_eq!(draft.values(), None);
        assert_eq!(draft.fields, before);
    }

    /// In the Values stage the last ticked value may be unticked — an
    /// emptied selection is "drop this dimension", not an invalid object
    /// — where the same untick on a Groupings chain is refused.
    #[test]
    fn the_last_tick_may_be_removed_in_the_values_stage_alone() {
        let mut draft = groupings_draft();
        let values = vec![Field {
            key: "values".to_string(),
            label: "Values".to_string(),
            kind: FieldKind::OrderedList {
                items: vec![ListItem { included: true, ..item("BK001") }],
                available: None,
            },
            dest: Destination::Doc,
            layer: None,
        }];
        assert!(draft.enter_values("book", values));
        draft.selected = 1; // the one item row under the header
        assert_eq!(draft.toggle_selected(), Step::Changed);
        assert!(!draft.list_items("values").unwrap()[0].included);
    }

    /// A stage with a previous rung: `escape` from Values steps back.
    #[test]
    fn values_is_a_stage_escape_can_step_back_from() {
        let mut state = ObjectDialogState::new(Domain::Scopes);
        state.stage = Stage::Values {
            object: "mine".into(),
            column: "book".into(),
        };
        assert!(state.has_previous_stage());
    }
```

`groupings_draft()` and `item()` are existing helpers in that test module (`item` builds a `ListItem` with `included: false` — update it to set `note: None` in Step 4).

- [ ] **Step 3: Run to verify they fail**

Run: `cargo test -p geode-shell objectdialog::tests::entering_values -- --nocapture`
Expected: compile error — `enter_values`, `values`, `leave_values`, `Stage::Values` and `ListItem.note` do not exist.

- [ ] **Step 4: Implement**

In `mod.rs`:

1. `ListItem` gains, after `kind`:
   ```rust
       /// Muted text painted after the name, supplied by the domain
       /// (Scopes: a selection's values on the edit stage, a value's row
       /// count or `not in data` on the Values stage). `None` paints
       /// Views' own `column_summary` as before. Display only — never
       /// written, never filtered on.
       pub note: Option<String>,
   ```
   Add `note: None` to every `ListItem { .. }` literal in `mod.rs` (tests included), `views.rs`, `groupings.rs`, `dataset_columns.rs` (`rg "ListItem \{" crates/geode-shell/src/shell/objectdialog` lists them).

2. `Stage` gains:
   ```rust
       /// Ticking one dimension's values for a saved scope (scopes-editing
       /// spec §4) — a projection over the same [`Draft`] in
       /// [`Stage::Column`]'s mould: `enter` on a `dimensions` row stashes
       /// the scope's fields and installs one list of the column's distinct
       /// values; `escape` restores the scope with the cursor on the column.
       Values {
           object: String,
           column: String,
       },
   ```
   and `has_previous_stage` matches `Stage::Values { .. }` too.

3. `Draft` gains, beside `column: Option<String>`:
   ```rust
       /// Which column [`Draft::parent_fields`] was stashed for by the
       /// VALUES stage (scopes-editing spec §4). `Some` exactly when
       /// `parent_fields` is and `column` is `None`; the two stages share
       /// the stash and can never both be open.
       values: Option<String>,
   ```
   Initialise `values: None` in every `Draft { .. }` literal (`draft()`, `new_object()`, the test literals — `rg "parent_fields: None" crates/geode-shell/src/shell/objectdialog` lists them).

4. Add beside `column()`/`enter_column()`/`leave_column()`:
   ```rust
       /// The column whose values are open, if [`Stage::Values`] is.
       pub fn values(&self) -> Option<&str> {
           self.values.as_deref()
       }

       /// Open the Values stage (scopes-editing spec §4): stash the object's
       /// fields, install `fields` (the one values list) as a clean
       /// baseline. Refused while any projection is already open, for
       /// `enter_column`'s reason — a second stash would drop the object's
       /// own fields for good.
       pub fn enter_values(&mut self, column: &str, fields: Vec<Field>) -> bool {
           if self.column.is_some() || self.values.is_some() {
               return false;
           }
           self.parent_fields = Some(std::mem::replace(&mut self.fields, fields));
           self.values = Some(column.to_string());
           self.baseline = self.fields.clone();
           self.query.clear();
           self.selected = 0;
           self.text_entry = None;
           true
       }

       /// Close the Values stage: restore the object's fields and hand the
       /// stage's own fields back for the adapter's fold
       /// (`scopes::fold_values` has already run on every tick through
       /// `render::revalidate`; the return is for the caller's final fold
       /// and cursor placement). The restored baseline is the restored
       /// fields, for `leave_column`'s reason. `None` when no Values stage
       /// was open.
       pub fn leave_values(&mut self) -> Option<Vec<Field>> {
           self.values.take()?;
           let parent = self.parent_fields.take()?;
           let own = std::mem::replace(&mut self.fields, parent);
           self.baseline = self.fields.clone();
           self.query.clear();
           self.text_entry = None;
           Some(own)
       }
   ```

5. In `step_selected`'s `EditRow::Item` arm, change the refusal guard to:
   ```rust
                   if included
                       && dest == Destination::Doc
                       && self.values.is_none()
                       && items.iter().filter(|i| i.included).count() == 1
                   {
   ```
   and add above it the comment: `// The Values stage may empty its list — that is "drop this dimension" (scopes-editing spec §4), folded by the adapter; the guard is a Groupings/Views rule.`

6. `field_by_key`'s parent fallback already covers the Values stage (it reads `parent_fields` whichever stage stashed them) — no change.

- [ ] **Step 5: Run the pure tests and the whole crate**

Run: `cargo test -p geode-shell objectdialog` then `cargo test -p geode-shell`
Expected: all pass (existing tests unchanged; `note: None` added wherever literals exist).

- [ ] **Step 6: Commit**

```bash
git add docs/superpowers/specs/2026-09-19-geode-scopes-dialog-editing-design.md crates/geode-shell/src/shell/objectdialog
git commit -m "objectdialog: ListItem.note, Draft.values and Stage::Values groundwork for scope editing

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 2: The Scopes adapter's three fields (pure)

**Files:**
- Modify: `crates/geode-shell/src/shell/objectdialog/scopes.rs` (module doc, `fields`, `fields_from_table`, `help`, new `fold`, `dimension_note`, `selected_columns`, `summary`)
- Modify: `crates/geode-shell/src/shell/objectdialog/mod.rs` (`Domain::text_editable`, `Domain::parse_text`, `Domain::fields` signature unchanged)
- Test: `scopes.rs` `mod tests`

**Interfaces:**
- Consumes: `crate::shell::pickable_columns(&Config) -> Vec<Pickable>` (existing, `shell/mod.rs`), `geode_core::scope::parse_expr`.
- Produces (used by Tasks 3–5):
  ```rust
  pub fn fields(config: &Config, object: Option<&str>) -> Vec<Field>;     // dimensions, text, expression
  pub fn fold(draft: &mut Draft);                                          // fields → source (text, expression, retained dimensions)
  pub fn dimension_note(values: &[String]) -> String;                      // "BK001, BK003" or "4 values"
  pub fn help(key: &str) -> &'static str;
  pub const NO_ORDER_NOTICE: &str = "selections have no order";
  ```

- [ ] **Step 1: Write the failing tests**

Replace the tests `fields_shows_selects_and_the_text_filter_read_only`, `an_object_that_does_not_exist_has_the_empty_fields_rather_than_panicking` and `neither_field_steps_with_space` in `scopes.rs` with:

```rust
    /// Three fields: the selected dimensions as a list with the other
    /// pickable columns available, then the text filter and the
    /// expression as editable text.
    #[test]
    fn fields_are_the_dimensions_list_the_text_filter_and_the_expression() {
        let config = config_with_scope(
            "[mine]\ntext = \"spx\"\nexpression = \"npv > 0\"\n[mine.dimensions]\nbook = [\"BK001\", \"BK003\"]\n\n[bare]\n",
        );
        let fields = Domain::Scopes.fields(&config, Some("mine"));
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0].key, "dimensions");
        let FieldKind::OrderedList { items, available } = &fields[0].kind else {
            panic!("dimensions is a list");
        };
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "book");
        assert!(items[0].included);
        assert_eq!(items[0].note.as_deref(), Some("BK001, BK003"));
        // `book` is the fixture's only categorical column; it is selected,
        // so the available catalogue exists and is empty.
        assert_eq!(available.as_deref(), Some(&[][..]));
        assert_eq!(fields[1].key, "text");
        assert_eq!(fields[1].kind, FieldKind::Text("spx".to_string()));
        assert_eq!(fields[2].key, "expression");
        assert_eq!(fields[2].kind, FieldKind::Text("npv > 0".to_string()));

        let bare = Domain::Scopes.fields(&config, Some("bare"));
        let FieldKind::OrderedList { items, available } = &bare[0].kind else {
            panic!("dimensions is a list");
        };
        assert!(items.is_empty());
        assert_eq!(available.as_ref().unwrap()[0].name, "book");
        assert_eq!(bare[1].kind, FieldKind::Text(String::new()));
        assert_eq!(bare[2].kind, FieldKind::Text(String::new()));
    }

    #[test]
    fn an_object_that_does_not_exist_has_the_empty_fields_rather_than_panicking() {
        let config = config_with_scope("[mine]\n[mine.dimensions]\nbook = [\"BK001\"]\n");
        for object in [None, Some("nonesuch")] {
            let fields = Domain::Scopes.fields(&config, object);
            assert!(matches!(&fields[0].kind, FieldKind::OrderedList { items, .. } if items.is_empty()));
            assert_eq!(fields[1].kind, FieldKind::Text(String::new()));
        }
    }

    /// The note after a selection's name: the values up to three, then a
    /// count.
    #[test]
    fn a_dimension_note_lists_up_to_three_values_then_counts() {
        assert_eq!(dimension_note(&["BK001".into()]), "BK001");
        assert_eq!(
            dimension_note(&["A".into(), "B".into(), "C".into()]),
            "A, B, C"
        );
        assert_eq!(
            dimension_note(&["A".into(), "B".into(), "C".into(), "D".into()]),
            "4 values"
        );
    }

    /// `i` opens `text` and `expression`; a bad expression is refused
    /// with the parser's own message, an empty one clears the key.
    #[test]
    fn text_and_expression_are_editable_and_the_expression_is_parsed() {
        assert!(Domain::Scopes.text_editable("text"));
        assert!(Domain::Scopes.text_editable("expression"));
        assert!(!Domain::Scopes.text_editable("dimensions"));
        assert_eq!(
            Domain::Scopes.parse_text("expression", " npv > 0 "),
            Ok("npv > 0".to_string())
        );
        assert_eq!(Domain::Scopes.parse_text("expression", ""), Ok(String::new()));
        let err = Domain::Scopes
            .parse_text("expression", "npv >")
            .unwrap_err();
        assert!(err.starts_with("expression: "), "{err}");
        assert_eq!(Domain::Scopes.parse_text("text", " spx "), Ok("spx".to_string()));
    }

    /// `fold` writes the two text fields into `source` and drops a
    /// dimension the list no longer names; it never touches a selection's
    /// values (those belong to the Values stage).
    #[test]
    fn fold_writes_text_expression_and_retained_dimensions_into_source() {
        let config = config_with_scope(
            "[mine]\ntext = \"spx\"\n[mine.dimensions]\nbook = [\"BK001\"]\nlhu = [\"L1\"]\n",
        );
        let mut draft = Domain::Scopes.draft(&config, "mine");
        // Type a new filter and an expression.
        draft.fields[1].kind = FieldKind::Text("ndx".to_string());
        draft.fields[2].kind = FieldKind::Text("npv > 0".to_string());
        // Drop `lhu` from the list, as `x` does.
        if let FieldKind::OrderedList { items, .. } = &mut draft.fields[0].kind {
            items.retain(|i| i.name != "lhu");
        }
        fold(&mut draft);
        assert_eq!(draft.source["text"].as_str(), Some("ndx"));
        assert_eq!(draft.source["expression"].as_str(), Some("npv > 0"));
        let dims = draft.source["dimensions"].as_table().unwrap();
        assert!(dims.contains_key("book"));
        assert!(!dims.contains_key("lhu"));
        assert_eq!(
            dims["book"].as_array().unwrap()[0].as_str(),
            Some("BK001"),
            "the fold never rewrites a kept selection's values"
        );
        assert!(draft.is_dirty());
    }
```

Keep every other existing test (`summary`, `overwrite_with_*`, `to_table_round_trips_*`, `domain_scopes_lists_*`) — they still hold. `config_with_scope`'s `datasets` doc must declare `book` categorical for `pickable_columns` to list it: change its `book` column to `type = "utf8"\nrole = "dimension"\ncategorical = true` (a `utf8` dimension is categorical by default per CLAUDE.md, so the explicit key is documentation; keep it).

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-shell objectdialog::scopes`
Expected: compile errors (`dimension_note`, `fold` missing) and assertion failures on `fields`.

- [ ] **Step 3: Implement the adapter**

In `scopes.rs`, replace `fields_from_table` and `fields` with:

```rust
/// The values a scope's raw table selects on `column`, in file order.
fn values_of(table: Option<&toml::Table>, column: &str) -> Vec<String> {
    table
        .and_then(|t| t.get("dimensions"))
        .and_then(|v| v.as_table())
        .and_then(|d| d.get(column))
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

/// What a selection's row says after its column name: the values
/// themselves up to three, a count past that.
pub fn dimension_note(values: &[String]) -> String {
    if values.len() <= 3 {
        values.join(", ")
    } else {
        format!("{} values", values.len())
    }
}

/// The scope's three fields (spec §3): its selected dimensions as a list
/// (one item per non-empty selection, the other pickable columns
/// available), then `text` and `expression` as editable text.
pub fn fields(config: &Config, object: Option<&str>) -> Vec<Field> {
    let table = object
        .and_then(|name| config.doc(DOC).and_then(|doc| doc.value.get(name)))
        .and_then(|value| value.as_table());
    fields_from_table(config, table)
}

fn fields_from_table(config: &Config, table: Option<&toml::Table>) -> Vec<Field> {
    let mut items = Vec::new();
    if let Some(dims) = table.and_then(|t| t.get("dimensions")).and_then(|v| v.as_table()) {
        for (column, _) in dims {
            let values = values_of(table, column);
            if values.is_empty() {
                continue;
            }
            items.push(ListItem {
                name: column.clone(),
                included: true,
                presentation: Default::default(),
                kind: None,
                note: Some(dimension_note(&values)),
            });
        }
    }
    let available: Vec<ListItem> = crate::shell::pickable_columns(config)
        .into_iter()
        .filter(|p| !items.iter().any(|i| i.name == p.column))
        .map(|p| ListItem {
            name: p.column,
            included: false,
            presentation: Default::default(),
            kind: None,
            note: None,
        })
        .collect();
    let text = |key: &str| {
        table
            .and_then(|t| t.get(key))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    vec![
        Field {
            key: "dimensions".to_string(),
            label: "Dimensions".to_string(),
            kind: FieldKind::OrderedList {
                items,
                available: Some(available),
            },
            dest: Destination::Doc,
            layer: None,
        },
        Field {
            key: "text".to_string(),
            label: "Text filter".to_string(),
            kind: FieldKind::Text(text("text")),
            dest: Destination::Doc,
            layer: None,
        },
        Field {
            key: "expression".to_string(),
            label: "Expression".to_string(),
            kind: FieldKind::Text(text("expression")),
            dest: Destination::Doc,
            layer: None,
        },
    ]
}

/// The notice `shift+j`/`shift+k` answer on this domain (ruling 4).
pub const NO_ORDER_NOTICE: &str = "selections have no order";

/// Fields → `source` (spec §3): the two text keys as typed, and
/// `dimensions` retained to the columns the list still names. A kept
/// selection's VALUES are never rewritten here — the Values stage owns
/// them (`fold_values`, Task 3). Called from `render::revalidate` on
/// every Scopes change, ahead of the validator and the writer.
pub fn fold(draft: &mut Draft) {
    let mut text = None;
    let mut expression = None;
    // `None` while the `dimensions` field is not installed — the Values
    // stage has stashed it — so the fold never retains against an empty
    // list and wipes every selection (the trap CLAUDE.md records).
    let mut kept: Option<Vec<String>> = None;
    for field in &draft.fields {
        match (field.key.as_str(), &field.kind) {
            ("text", FieldKind::Text(t)) => text = Some(t.clone()),
            ("expression", FieldKind::Text(e)) => expression = Some(e.clone()),
            ("dimensions", FieldKind::OrderedList { items, .. }) => {
                kept = Some(items.iter().map(|i| i.name.clone()).collect());
            }
            _ => {}
        }
    }
    if let Some(text) = text {
        draft.source.insert("text".into(), toml::Value::String(text));
    }
    if let Some(expression) = expression {
        draft
            .source
            .insert("expression".into(), toml::Value::String(expression));
    }
    if let Some(kept) = kept {
        let dims = draft
            .source
            .entry("dimensions".to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let Some(dims) = dims.as_table_mut() {
            dims.retain(|column, _| kept.iter().any(|k| k == column));
        }
    }
}
```

Add to the tests a guard for that `None` branch:

```rust
    /// While the Values stage has stashed the `dimensions` field, `fold`
    /// leaves `source.dimensions` alone rather than retaining it against
    /// an empty list.
    #[test]
    fn fold_leaves_dimensions_alone_while_the_values_stage_is_open() {
        let config = config_with_scope("[mine]\n[mine.dimensions]\nbook = [\"BK001\"]\n");
        let mut draft = Domain::Scopes.draft(&config, "mine");
        assert!(draft.enter_values("book", Vec::new()));
        fold(&mut draft);
        assert!(draft.source["dimensions"].as_table().unwrap().contains_key("book"));
    }
```

`overwrite_with` now needs the config for the available block: change its signature to `pub fn overwrite_with(draft: &mut Draft, scope: &Scope, config: &Config)` and its body's second line to `draft.fields = fields_from_table(config, Some(&draft.source));`. Update its two callers in `render.rs` (`run_confirmed`'s `Confirm::Overwrite` arm and `run_overwrite`; both have `shell.services.config` in hand) and its tests.

`help` becomes:

```rust
pub fn help(key: &str) -> &'static str {
    match key {
        "dimensions" => "The dimensions this scope narrows — open one to tick its values, x drops it",
        "text" => "A text filter matched against every textual column; empty for none",
        "expression" => "A filter expression over the scope's columns, checked when applied; empty for none",
        "values" => "The values this dimension keeps — every row counts what the scope would leave",
        _ => "",
    }
}
```

Add `use super::ListItem;` to the imports. Update the module doc: replace "## The thinnest adapter on purpose" through the `o` section's first paragraph with a short statement that the adapter edits all three keys (spec `2026-09-19-geode-scopes-dialog-editing-design.md`), that `to_table` still renders `source` and `fold`/`fold_values` are the only writers of it, and that `o` is the frame door.

In `mod.rs`, `Domain::text_editable`:
```rust
            Domain::Groupings | Domain::Colours => { let _ = key; false }
            Domain::Scopes => matches!(key, "text" | "expression"),
```
and `Domain::parse_text`:
```rust
            Domain::Groupings | Domain::Colours => { let _ = key; Ok(text.trim().to_string()) }
            Domain::Scopes => scopes::parse_text(key, text),
```
with, in `scopes.rs`:
```rust
/// `i`'s commit door (spec §5): `text` trims; `expression` must parse —
/// a broken expression is refused with the parser's own message rather
/// than written for the loader to warn about and drop.
pub fn parse_text(key: &str, text: &str) -> Result<String, String> {
    let text = text.trim();
    if key == "expression" && !text.is_empty() {
        geode_core::scope::parse_expr(text).map_err(|e| format!("expression: {e}"))?;
    }
    Ok(text.to_string())
}
```

Update `section_header_text` in `render.rs`: the `Domain::Scopes` arm becomes two — `(Domain::Scopes, true) => ("DIMENSIONS — enter opens values · x drops", "members")` and `(Domain::Scopes, false) => ("AVAILABLE — enter picks values", "available")`; keep the remaining three domains in the empty arm and fix its comment.

- [ ] **Step 4: Wire the fold into `revalidate`**

In `render.rs` `revalidate`, after the `fold_column` block:
```rust
    // Scopes: fields → source on every change (scopes-editing spec §3),
    // so the validator and the writer read this keystroke. The Values
    // stage's own fold is Task 3's `fold_values`, dispatched here too.
    if domain == Domain::Scopes {
        scopes::fold(draft);
    }
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p geode-shell objectdialog` then `cargo test -p geode-shell`
Expected: PASS. The window test `every_field_on_every_domain_has_help` (`shell/tests/objectdialog.rs`) expects a `selects` row on Scopes — change its case to `("config::scopes", services_with_a_saved_scope, "dimensions")`. The existing `o_*` and `n_on_a_scope_saves_the_frames_current_scope` window tests still pass (`n` changes in Task 5).

- [ ] **Step 6: Commit**

```bash
git add crates/geode-shell/src/shell
git commit -m "scopes dialog: dimensions list, editable text and expression, fold into source

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 3: The Values stage's pure half and the distinct routing

**Files:**
- Modify: `crates/geode-shell/src/shell/objectdialog/scopes.rs` (`values_fields`, `fold_values`, `draft_scope`)
- Modify: `crates/geode-shell/src/shell/mod.rs` (`SCOPES_KEY`, `deliver_distinct` routing)
- Modify: `crates/geode-shell/src/shell/objectdialog/mod.rs` (`ObjectDialogState.values_tag`)
- Modify: `crates/geode-shell/src/shell/objectdialog/render.rs` (`deliver_values`)
- Test: `scopes.rs` `mod tests`; `crates/geode-shell/src/shell/tests/objectdialog.rs`

**Interfaces:**
- Consumes: `geode_core::query::{DistinctOutcome, QueryKey}`, `PICKER_KEY` (`shell/mod.rs`), Task 1's `Draft::enter_values`/`values`.
- Produces:
  ```rust
  pub const SCOPES_KEY: QueryKey = QueryKey(u64::MAX - 3);                 // shell/mod.rs
  pub fn loading_field() -> Vec<Field>;                                     // scopes.rs: one display Text "loading…"
  pub fn failed_field(message: &str) -> Vec<Field>;                         // one display Text with the failure
  pub fn values_fields(saved: &[String], values: &[(String, u64)]) -> Vec<Field>; // the ticked list
  pub fn fold_values(draft: &mut Draft);                                    // values list → source.dimensions.<col> and the parent's items
  pub fn draft_scope(draft: &Draft, config: &Config, minus: &str) -> Scope; // the draft's scope without `minus`
  pub(crate) fn deliver_values(shell: &mut ShellView, outcome: DistinctOutcome, cx: &mut Context<ShellView>); // render.rs
  pub values_tag: u64  // on ObjectDialogState
  ```

- [ ] **Step 1: Write the failing pure tests** (append to `scopes.rs` tests)

```rust
    /// The values list: every delivered value with its count, ticked iff
    /// saved; then every saved value the data lacks, ticked and marked.
    #[test]
    fn values_fields_tick_the_saved_ones_and_keep_a_stale_one_marked() {
        let saved = vec!["BK001".to_string(), "BK009".to_string()];
        let fields = values_fields(&saved, &[("BK000".into(), 5), ("BK001".into(), 7)]);
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].key, "values");
        let FieldKind::OrderedList { items, available } = &fields[0].kind else {
            panic!("a list");
        };
        assert!(available.is_none(), "ticking is membership here");
        let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["BK000", "BK001", "BK009"]);
        assert!(!items[0].included);
        assert_eq!(items[0].note.as_deref(), Some("5"));
        assert!(items[1].included);
        assert!(items[2].included);
        assert_eq!(items[2].note.as_deref(), Some("not in data"));
    }

    /// `fold_values` writes the ticked values under the column; an
    /// emptied selection removes the key (never an empty array) and the
    /// parent's `dimensions` list follows in both directions.
    #[test]
    fn fold_values_writes_the_ticks_and_removes_an_emptied_selection() {
        let config = config_with_scope("[mine]\n[mine.dimensions]\nbook = [\"BK001\"]\n");
        let mut draft = Domain::Scopes.draft(&config, "mine");
        assert!(draft.enter_values(
            "book",
            values_fields(&["BK001".into()], &[("BK000".into(), 5), ("BK001".into(), 7)])
        ));
        // Tick BK000 too.
        draft.selected = 1;
        assert_eq!(draft.toggle_selected(), Step::Changed);
        fold_values(&mut draft);
        let dims = draft.source["dimensions"].as_table().unwrap();
        let book: Vec<&str> = dims["book"].as_array().unwrap().iter().filter_map(|v| v.as_str()).collect();
        assert_eq!(book, ["BK000", "BK001"]);
        // The parent list (stashed) already carries the new note.
        let own = draft.leave_values().unwrap();
        assert_eq!(draft.list_items("dimensions").unwrap()[0].note.as_deref(), Some("BK000, BK001"));

        // Now untick everything: the key goes, the column returns to
        // the available block.
        assert!(draft.enter_values("book", own));
        for row in [1usize, 2] {
            draft.selected = row;
            let _ = draft.toggle_selected();
        }
        fold_values(&mut draft);
        assert!(!draft.source["dimensions"].as_table().unwrap().contains_key("book"));
        draft.leave_values();
        assert!(draft.list_items("dimensions").unwrap().is_empty());
        assert!(draft.available_items("dimensions").unwrap().iter().any(|i| i.name == "book"));
        assert!(draft.is_dirty());
    }

    /// A first tick on a column that was only available inserts the
    /// selection — and the item — where none existed.
    #[test]
    fn a_first_tick_inserts_a_new_selection() {
        let config = config_with_scope("[mine]\n[mine.dimensions]\n");
        let mut draft = Domain::Scopes.draft(&config, "mine");
        assert!(draft.enter_values("book", values_fields(&[], &[("BK000".into(), 5)])));
        draft.selected = 1;
        assert_eq!(draft.toggle_selected(), Step::Changed);
        fold_values(&mut draft);
        assert_eq!(
            draft.source["dimensions"]["book"].as_array().unwrap()[0].as_str(),
            Some("BK000")
        );
        draft.leave_values();
        assert_eq!(draft.list_items("dimensions").unwrap()[0].name, "book");
        assert!(draft.available_items("dimensions").unwrap().iter().all(|i| i.name != "book"));
    }

    /// The scope the distinct request carries is the DRAFT's, minus the
    /// column being asked about.
    #[test]
    fn draft_scope_is_the_drafts_own_minus_the_column() {
        let config = config_with_scope(
            "[mine]\ntext = \"spx\"\n[mine.dimensions]\nbook = [\"BK001\"]\nlhu = [\"L1\"]\n",
        );
        let draft = Domain::Scopes.draft(&config, "mine");
        let scope = draft_scope(&draft, &config, "book");
        assert_eq!(scope.text.as_deref(), Some("spx"));
        assert_eq!(scope.dimensions.len(), 1);
        assert_eq!(scope.dimensions[0].column, "lhu");
    }
```

`config_with_scope`'s datasets doc must also declare `lhu` as a dimension for the last test (`[risk.columns.lhu]\ntype = "utf8"\nrole = "dimension"\n`).

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-shell objectdialog::scopes`
Expected: compile errors — `values_fields`, `fold_values`, `draft_scope` missing.

- [ ] **Step 3: Implement the pure half** (`scopes.rs`)

```rust
/// The Values stage's one field before the data answers (spec §4): a
/// display-only row, so the stage has a shape to paint and `escape` to
/// leave by. Replaced whole by [`values_fields`] on delivery.
pub fn loading_field() -> Vec<Field> {
    status_field("loading…")
}

/// The Values stage's one field when the request failed: the failure
/// text on the row, `escape` the way out.
pub fn failed_field(message: &str) -> Vec<Field> {
    status_field(message)
}

fn status_field(text: &str) -> Vec<Field> {
    vec![Field {
        key: "values".to_string(),
        label: "Values".to_string(),
        kind: FieldKind::Text(text.to_string()),
        dest: Destination::Doc,
        layer: None,
    }]
}

/// The Values stage's list (spec §4): one row per delivered `(value,
/// count)` in the outcome's own order, ticked iff `saved` lists it, the
/// count as its note; then every saved value the data does NOT hold,
/// ticked, noted `not in data` (ruling 5) — visible and untickable,
/// never silently dropped.
pub fn values_fields(saved: &[String], values: &[(String, u64)]) -> Vec<Field> {
    let mut items: Vec<ListItem> = values
        .iter()
        .map(|(value, count)| ListItem {
            name: value.clone(),
            included: saved.iter().any(|s| s == value),
            presentation: Default::default(),
            kind: None,
            note: Some(count.to_string()),
        })
        .collect();
    for value in saved {
        if !values.iter().any(|(v, _)| v == value) {
            items.push(ListItem {
                name: value.clone(),
                included: true,
                presentation: Default::default(),
                kind: None,
                note: Some("not in data".to_string()),
            });
        }
    }
    vec![Field {
        key: "values".to_string(),
        label: "Values".to_string(),
        kind: FieldKind::OrderedList {
            items,
            available: None,
        },
        dest: Destination::Doc,
        layer: None,
    }]
}

/// The Values stage's fold (spec §4): the ticked values become
/// `source.dimensions.<column>` — the key removed outright when none is
/// ticked, since an empty array is never written — and the stashed
/// parent's `dimensions` list follows: a first tick inserts the item
/// (out of the available block), an emptied selection returns it there,
/// a changed selection refreshes the note. Called from
/// `render::revalidate` on every tick while [`Draft::values`] is `Some`;
/// a no-op while the stage still shows its loading/failed row.
pub fn fold_values(draft: &mut Draft) {
    let Some(column) = draft.values().map(str::to_string) else {
        return;
    };
    let Some(items) = draft.list_items("values") else {
        return;
    };
    let ticked: Vec<String> = items
        .iter()
        .filter(|i| i.included)
        .map(|i| i.name.clone())
        .collect();
    let dims = draft
        .source
        .entry("dimensions".to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if let Some(dims) = dims.as_table_mut() {
        if ticked.is_empty() {
            dims.remove(&column);
        } else {
            dims.insert(
                column.clone(),
                toml::Value::Array(ticked.iter().cloned().map(toml::Value::String).collect()),
            );
        }
    }
    draft.with_parent_list("dimensions", |items, available| {
        let position = items.iter().position(|i| i.name == column);
        match (position, ticked.is_empty()) {
            (Some(i), true) => {
                let mut entry = items.remove(i);
                entry.included = false;
                entry.note = None;
                if let Some(available) = available {
                    available.push(entry);
                }
            }
            (Some(i), false) => items[i].note = Some(dimension_note(&ticked)),
            (None, false) => {
                let mut entry = available
                    .as_mut()
                    .and_then(|a| {
                        a.iter().position(|i| i.name == column).map(|p| a.remove(p))
                    })
                    .unwrap_or_else(|| ListItem {
                        name: column.clone(),
                        included: true,
                        presentation: Default::default(),
                        kind: None,
                        note: None,
                    });
                entry.included = true;
                entry.note = Some(dimension_note(&ticked));
                items.push(entry);
            }
            (None, true) => {}
        }
    });
}

/// The draft's scope as `saved_scopes_from_doc` would read it, with
/// `minus`'s own selection removed — what the distinct request carries
/// (spec §4), so a value's count answers "within the scope I am
/// authoring". An unreadable draft (the reader warns and drops it)
/// yields the empty scope: the counts are then dataset-wide, which is
/// honest for a scope that does not yet parse.
pub fn draft_scope(draft: &Draft, config: &Config, minus: &str) -> Scope {
    let table = rendered_doc_table(draft);
    let doc = merge_docs(
        DOC,
        &[LayerDoc {
            layer: Layer::User,
            name: DOC.to_string(),
            file: std::path::PathBuf::from("<draft>"),
            table,
        }],
    );
    let (schema, _) = config
        .doc("datasets")
        .map(SchemaSpec::from_doc)
        .unwrap_or_default();
    let (dims, _) = config
        .doc("dimensions")
        .map(DerivedDimensions::from_doc)
        .unwrap_or_default();
    let (mut saved, _) = saved_scopes_from_doc(&doc, &schema, &dims);
    let mut scope = saved.remove(&draft.name).unwrap_or_default();
    scope.dimensions.retain(|d| d.column != minus);
    scope
}
```

`Draft::with_parent_list` is a small new door in `mod.rs` (beside `field_by_key`), because `parent_fields` is private:

```rust
    /// Mutate one of the STASHED parent's ordered lists while a projection
    /// is open — the Values stage's fold writes the scope's `dimensions`
    /// list through here. A no-op when no stash or no such list exists.
    pub fn with_parent_list(
        &mut self,
        key: &str,
        f: impl FnOnce(&mut Vec<ListItem>, &mut Option<Vec<ListItem>>),
    ) {
        let Some(parent) = self.parent_fields.as_mut() else {
            return;
        };
        let Some(field) = parent.iter_mut().find(|fld| fld.key == key) else {
            return;
        };
        if let FieldKind::OrderedList { items, available } = &mut field.kind {
            f(items, available);
        }
    }
```

Add `use geode_core::query::…` only where needed (none in this file). Wire `fold_values` into `revalidate` next to `fold`:
```rust
    if domain == Domain::Scopes {
        if draft.values().is_some() {
            scopes::fold_values(draft);
        }
        scopes::fold(draft);
    }
```
`fold` runs after `fold_values`; inside the Values stage the `dimensions` field is stashed, so Task 2's `kept: Option<..>` guard is what keeps `fold` from retaining against an empty list — `fold_leaves_dimensions_alone_while_the_values_stage_is_open` pins it.

- [ ] **Step 4: The key and the routing** (`shell/mod.rs`)

Beside `PICKER_KEY`:
```rust
/// The Scopes dialog's Values stage submits its `Request::Distinct` under
/// this key (scopes-editing spec §4) — one lower than `DIAGNOSTICS_KEY`,
/// same reservation reasoning. `deliver_distinct` routes on it.
pub const SCOPES_KEY: QueryKey = QueryKey(u64::MAX - 3);
```
`deliver_distinct` becomes:
```rust
    pub fn deliver_distinct(&mut self, outcome: DistinctOutcome, cx: &mut Context<Self>) {
        if outcome.key == SCOPES_KEY {
            shell::objectdialog::render::deliver_values(self, outcome, cx);
            return;
        }
        let Some(state) = self.picker.as_mut() else { return; };
        // … existing body unchanged …
    }
```
(`render` is a private module of `objectdialog`; expose `deliver_values` as `pub(in crate::shell)` and re-export it from `objectdialog/mod.rs` as `pub(in crate::shell) use render::deliver_values;`.)

`ObjectDialogState` gains `pub values_tag: u64` (initialised `0` in `new`), with the doc: "The tag of the latest distinct request the Values stage submitted (`render::enter_values_stage`); an outcome with any other tag is stale and dropped (spec §7.3)."

- [ ] **Step 5: `deliver_values`** (`render.rs`)

```rust
/// A `DistinctOutcome` addressed to `SCOPES_KEY`, routed here by
/// `ShellView::deliver_distinct`. Applied only when a Scopes dialog is
/// open in the Values stage for `outcome.column` and the tag is the
/// latest one handed out — the picker's own three guards, so a reply to
/// a stage the trader has already left, or to a superseded request,
/// changes nothing. `Ok` installs the ticked list as a CLEAN baseline
/// (delivered ticks are the saved scope, not dirt); `Err` installs the
/// failure row.
pub(in crate::shell) fn deliver_values(
    shell: &mut ShellView,
    outcome: DistinctOutcome,
    cx: &mut Context<ShellView>,
) {
    let Some(state) = shell.object_dialog.as_mut() else {
        return;
    };
    if state.domain != Domain::Scopes {
        return;
    }
    let Stage::Values { column, .. } = &state.stage else {
        return;
    };
    if *column != outcome.column || outcome.tag != state.values_tag {
        return;
    }
    let Some(draft) = state.draft.as_mut() else {
        return;
    };
    let saved: Vec<String> = draft
        .source
        .get("dimensions")
        .and_then(|v| v.as_table())
        .and_then(|d| d.get(&outcome.column))
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let fields = match &outcome.values {
        Ok(values) => scopes::values_fields(&saved, values),
        Err(message) => scopes::failed_field(message),
    };
    draft.reseed_fields(fields);
    draft.selected = 0;
    shell.object_dialog_scroll.scroll_to_item(0);
    cx.notify();
}
```
`Draft::reseed_fields` exists (used by `leave_column_stage` for Schema); confirm it sets both `fields` and `baseline`.

- [ ] **Step 6: Window test for the routing** (`shell/tests/objectdialog.rs`)

```rust
/// A `SCOPES_KEY` outcome reaches the Values stage; a `PICKER_KEY` one
/// never does, and a stale tag or a different column is dropped.
#[gpui::test]
fn deliver_values_routes_by_key_and_drops_stale_outcomes(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_saved_scope(),
        dir.path(),
        "config::scopes",
    );
    cx.simulate_keystrokes("enter"); // open `mine`
    cx.simulate_keystrokes("j");     // onto the `book` item row
    cx.simulate_keystrokes("enter"); // Values stage (Task 4's door)
    cx.run_until_parked();
    let tag = dialog_state(&shell, &cx, |s| s.values_tag);
    let deliver = |shell: &Entity<ShellView>, cx: &mut gpui::VisualTestContext, key, tag, column: &str| {
        shell.update(cx, |s, cx| {
            s.deliver_distinct(
                DistinctOutcome {
                    key,
                    tag,
                    column: column.into(),
                    values: Ok(vec![("BK000".into(), 5), ("BK001".into(), 7)]),
                },
                cx,
            )
        });
        cx.run_until_parked();
    };
    let still_loading = |shell: &Entity<ShellView>, cx: &gpui::VisualTestContext| {
        edit_draft(shell, cx, |d| matches!(&d.fields[0].kind, FieldKind::Text(t) if t == "loading…"))
    };
    deliver(&shell, &mut cx, PICKER_KEY, tag, "book");
    assert!(still_loading(&shell, &cx), "the picker's key never reaches the dialog");
    deliver(&shell, &mut cx, SCOPES_KEY, tag.wrapping_sub(1), "book");
    assert!(still_loading(&shell, &cx), "a stale tag is dropped");
    deliver(&shell, &mut cx, SCOPES_KEY, tag, "lhu");
    assert!(still_loading(&shell, &cx), "another column's answer is dropped");
    deliver(&shell, &mut cx, SCOPES_KEY, tag, "book");
    let names: Vec<String> = edit_draft(&shell, &cx, |d| {
        d.list_items("values").unwrap().iter().map(|i| i.name.clone()).collect()
    });
    assert_eq!(names, ["BK000", "BK001"]);
    assert!(edit_draft(&shell, &cx, |d| d.list_items("values").unwrap()[1].included));
    assert!(!edit_draft(&shell, &cx, |d| d.is_dirty()), "a delivery is not dirt");
}
```
This test compiles now but passes only after Task 4's door exists; write it here, run it in Task 4. Imports needed at the top of the test file: `use crate::shell::{PICKER_KEY, SCOPES_KEY}; use geode_core::query::DistinctOutcome; use crate::shell::objectdialog::FieldKind;` (check what is already imported).

- [ ] **Step 7: Run the pure tests, then commit**

Run: `cargo test -p geode-shell objectdialog::scopes && cargo test -p geode-shell objectdialog::tests`
Expected: PASS (the window test is deferred to Task 4).

```bash
git add crates/geode-shell/src/shell
git commit -m "scopes dialog: values-stage pure half (values_fields, fold_values, draft_scope) and SCOPES_KEY routing

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 4: The Values stage in the dialog — doors, keys, footer, painting

**Files:**
- Modify: `crates/geode-shell/src/shell/objectdialog/render.rs` (`values_stage_target`, `enter_values_stage`, `leave_values_stage`, `commit_selected_row`, `on_edit_row_clicked`, `step_selected_row`, `handle_edit_key`, the escape ladder, `crumb_text`, `build_edit`'s footer and item row, `actions`, `not_a_column_verb` siblings)
- Modify: `crates/geode-shell/src/shell/objectdialog/mod.rs` (`Domain::help` for `Stage::Values`)
- Test: `crates/geode-shell/src/shell/tests/objectdialog.rs`

**Interfaces:**
- Consumes: Task 3's `scopes::{loading_field, draft_scope, NO_ORDER_NOTICE}`, `SCOPES_KEY`, `ShellView::next_picker_tag`, `ShellEvent::DistinctRequested`, `geode_core::query::DistinctParams`.
- Produces: `fn values_stage_target(shell) -> Option<String>`, `fn enter_values_stage(shell, column, cx)`, `fn leave_values_stage(shell, cx)`.

- [ ] **Step 1: Write the failing window tests** (append to `shell/tests/objectdialog.rs`)

```rust
/// `enter` on a `dimensions` row opens the Values stage and asks the
/// data for that column's values, carrying the DRAFT's scope minus the
/// column; `space` on an available row opens it too; `escape` returns to
/// the scope with the cursor on the column.
#[gpui::test]
fn entering_the_values_stage_requests_the_columns_distinct_values(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_saved_scope(),
        dir.path(),
        "config::scopes",
    );
    let requested = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    cx.update(|_, cx| {
        let requested = requested.clone();
        cx.subscribe(&shell, move |_, e: &ShellEvent, _| {
            if let ShellEvent::DistinctRequested(p) = e {
                requested.borrow_mut().push(p.clone());
            }
        })
        .detach();
    });
    cx.simulate_keystrokes("enter");
    cx.simulate_keystrokes("j");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Values { ref column, .. } if column == "book"
    ));
    let req = requested.borrow().last().cloned().expect("a distinct request");
    assert_eq!(req.key, SCOPES_KEY);
    assert_eq!(req.column, "book");
    assert!(req.scope.dimensions.is_empty(), "own selection removed from a one-dimension scope");
    assert_eq!(req.tag, dialog_state(&shell, &cx, |s| s.values_tag));
    assert!(edit_draft(&shell, &cx, |d| d.values() == Some("book")));

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit { .. }
    ));
    assert!(edit_draft(&shell, &cx, |d| d.values().is_none()));
    assert!(edit_draft(&shell, &cx, |d| matches!(
        d.selected_row(),
        Some(objectdialog::EditRow::Item { .. })
    )), "the cursor lands on the column's own row");
}

/// A tick in the Values stage writes the selection to disk through the
/// ordinary debounced path, and unticking every value removes the key.
#[gpui::test]
fn ticking_a_value_writes_the_selection_and_unticking_all_removes_it(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_saved_scope(),
        dir.path(),
        "config::scopes",
    );
    cx.simulate_keystrokes("enter");
    cx.simulate_keystrokes("j");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let tag = dialog_state(&shell, &cx, |s| s.values_tag);
    shell.update(&mut cx, |s, cx| {
        s.deliver_distinct(
            DistinctOutcome {
                key: SCOPES_KEY,
                tag,
                column: "book".into(),
                values: Ok(vec![("BK000".into(), 5), ("BK001".into(), 7)]),
            },
            cx,
        )
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("j");     // BK000
    cx.simulate_keystrokes("space"); // tick it
    cx.executor().advance_clock(std::time::Duration::from_millis(400));
    cx.run_until_parked();
    assert_eq!(
        saved_scope_books(&shell, &cx).as_deref(),
        Some(&["BK000".to_string(), "BK001".to_string()][..])
    );
    cx.simulate_keystrokes("space"); // untick BK000
    cx.simulate_keystrokes("j");
    cx.simulate_keystrokes("space"); // untick BK001
    cx.executor().advance_clock(std::time::Duration::from_millis(400));
    cx.run_until_parked();
    assert_eq!(saved_scope_books(&shell, &cx), None, "an emptied selection is removed, never []");
    let written = std::fs::read_to_string(dir.path().join("scopes.toml")).unwrap();
    assert!(!written.contains("book = []"), "{written}");
}

/// `ctrl+a` ticks every shown value, `ctrl+x` clears; `shift+j` refuses
/// with the no-order notice on both stages.
#[gpui::test]
fn ctrl_a_and_ctrl_x_tick_and_clear_and_reorder_is_refused(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_saved_scope(),
        dir.path(),
        "config::scopes",
    );
    cx.simulate_keystrokes("enter");
    cx.simulate_keystrokes("j");
    cx.simulate_keystrokes("shift-j");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.notice.clone()).as_deref(),
        Some("selections have no order")
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let tag = dialog_state(&shell, &cx, |s| s.values_tag);
    shell.update(&mut cx, |s, cx| {
        s.deliver_distinct(
            DistinctOutcome {
                key: SCOPES_KEY,
                tag,
                column: "book".into(),
                values: Ok(vec![("BK000".into(), 5), ("BK001".into(), 7), ("BK002".into(), 1)]),
            },
            cx,
        )
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("ctrl-a");
    cx.run_until_parked();
    assert!(edit_draft(&shell, &cx, |d| d.list_items("values").unwrap().iter().all(|i| i.included)));
    cx.simulate_keystrokes("ctrl-x");
    cx.run_until_parked();
    assert!(edit_draft(&shell, &cx, |d| d.list_items("values").unwrap().iter().all(|i| !i.included)));
    cx.simulate_keystrokes("j");
    cx.simulate_keystrokes("shift-j");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.notice.clone()).as_deref(),
        Some("selections have no order")
    );
}

/// `i` on the expression row refuses a broken expression and keeps the
/// field open; a good one is written.
#[gpui::test]
fn a_broken_expression_is_refused_and_a_good_one_is_written(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_saved_scope(),
        dir.path(),
        "config::scopes",
    );
    cx.simulate_keystrokes("enter");
    cx.simulate_keystrokes("shift-g"); // last row: expression
    cx.simulate_keystrokes("i");
    cx.simulate_input("npv >");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(edit_draft(&shell, &cx, |d| d.text_entry.is_some()), "the field stays open");
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone()).unwrap_or_default();
    assert!(notice.starts_with("expression: "), "{notice}");
    cx.simulate_input(" 0");
    cx.simulate_keystrokes("enter");
    cx.executor().advance_clock(std::time::Duration::from_millis(400));
    cx.run_until_parked();
    let written = std::fs::read_to_string(dir.path().join("scopes.toml")).unwrap();
    assert!(written.contains("expression = \"npv > 0\""), "{written}");
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-shell --features test-support entering_the_values_stage`
Expected: FAIL — `enter` on the item row gives the "nothing to open" notice; no request is emitted.

- [ ] **Step 3: The doors** (`render.rs`)

Beside `column_stage_target`:

```rust
/// The dimension the row under the cursor would open a Values stage for
/// (scopes-editing spec §3): a Scopes draft's `dimensions` item OR
/// available row, outside any projection. Read by `commit_selected_row`,
/// `on_edit_row_clicked` and the `space` arm alike, so the three doors
/// cannot disagree about which rows open values.
fn values_stage_target(shell: &ShellView) -> Option<String> {
    let state = shell.object_dialog.as_ref()?;
    if state.domain != Domain::Scopes {
        return None;
    }
    let draft = state.draft.as_ref()?;
    if draft.column().is_some() || draft.values().is_some() {
        return None;
    }
    match draft.selected_row()? {
        row @ (EditRow::Item { field, .. } | EditRow::Available { field, .. })
            if draft.fields.get(field).is_some_and(|f| f.key == "dimensions") =>
        {
            Some(draft.row_label(row))
        }
        _ => None,
    }
}
```

`commit_selected_row`:
```rust
fn commit_selected_row(shell: &mut ShellView, cx: &mut Context<ShellView>) {
    if let Some(name) = column_stage_target(shell) {
        enter_column_stage(shell, &name, cx);
    } else if let Some(column) = values_stage_target(shell) {
        enter_values_stage(shell, &column, cx);
    } else {
        edit_commit_notice(shell);
    }
}
```
`on_edit_row_clicked`: after the `column_stage_target` block add `else if let Some(column) = values_stage_target(shell) { enter_values_stage(shell, &column, cx); }`.

`enter_values_stage`, beside `enter_column_stage`:

```rust
/// **The one door into the Values stage** (scopes-editing spec §4):
/// stash the scope's fields, install the loading row, and ask the data
/// for `column`'s distinct values under `SCOPES_KEY` with the DRAFT's
/// scope minus this column. The tag comes from the shell's one monotonic
/// counter (`next_picker_tag`, Phase 4b M5's reasoning) and is recorded
/// on the state for `deliver_values`'s staleness check. Opens in normal
/// mode, for `enter_column_stage`'s reason.
fn enter_values_stage(shell: &mut ShellView, column: &str, cx: &mut Context<ShellView>) {
    let pending = apply::config_with_pending(shell);
    let config = pending.as_ref().unwrap_or(&shell.services.config);
    let Some(state) = shell.object_dialog.as_ref() else {
        return;
    };
    let Stage::Edit { object } = &state.stage else {
        return;
    };
    let object = object.clone();
    let Some(draft) = state.draft.as_ref() else {
        return;
    };
    let scope = scopes::draft_scope(draft, config, column);
    let as_of = shell.frame.read(cx).as_of().clone();
    shell.next_picker_tag += 1;
    let tag = shell.next_picker_tag;
    let Some(state) = shell.object_dialog.as_mut() else {
        return;
    };
    let Some(draft) = state.draft.as_mut() else {
        return;
    };
    if !draft.enter_values(column, scopes::loading_field()) {
        return;
    }
    state.stage = Stage::Values {
        object,
        column: column.to_string(),
    };
    state.mode = DialogMode::Normal;
    state.notice = None;
    state.disarm();
    state.values_tag = tag;
    shell.object_dialog_scroll.scroll_to_item(0);
    cx.emit(ShellEvent::DistinctRequested(DistinctParams {
        key: SCOPES_KEY,
        tag,
        column: column.to_string(),
        scope,
        as_of,
    }));
    cx.notify();
}

/// `escape` out of the Values stage: fold one last time, restore the
/// scope's fields, and put the cursor on the column's row — its item row
/// if the selection survived, its available row if it was emptied.
fn leave_values_stage(shell: &mut ShellView, cx: &mut Context<ShellView>) {
    let Some(state) = shell.object_dialog.as_mut() else {
        return;
    };
    let Stage::Values { object, column } = state.stage.clone() else {
        return;
    };
    if let Some(draft) = state.draft.as_mut() {
        scopes::fold_values(draft);
        draft.leave_values();
        let rows = draft.rows();
        let target = draft
            .visible_rows()
            .iter()
            .position(|m| {
                matches!(
                    rows.get(m.row),
                    Some(row @ (EditRow::Item { .. } | EditRow::Available { .. }))
                        if draft.row_label(*row) == column
                )
            })
            .unwrap_or(0);
        draft.selected = target;
    }
    state.stage = Stage::Edit { object };
    state.notice = None;
    state.disarm();
    scroll_to_cursor(shell);
    cx.notify();
}
```
Imports for `render.rs`: `use crate::shell::SCOPES_KEY; use geode_core::query::DistinctParams;` (check `ShellEvent` is already imported).

- [ ] **Step 4: Keys**

In `handle_edit_key`:

1. The escape ladder's `PreviousStage` arm: before `in_column`, add
   ```rust
                let in_values = shell
                    .object_dialog
                    .as_ref()
                    .is_some_and(|state| matches!(state.stage, Stage::Values { .. }));
                if in_values {
                    leave_values_stage(shell, cx);
                    return true;
                }
   ```
2. Normal mode, before `let Some(cmd) = dialogmode::normal_command(ks)`: the Values stage's two chords —
   ```rust
    // Values stage (scopes-editing spec §4): `ctrl+a` ticks every value
    // the filter shows, `ctrl+x` clears the selection — the picker's own
    // pair, reclaimed inside `GeodeModal` already.
    let in_values = draft_ref(shell).is_some_and(|d| d.values().is_some());
    if in_values && ks.mods == Modifiers::CTRL && (ks.key == "a" || ks.key == "x") {
        let tick_all = ks.key == "a";
        let changed = draft_mut(shell).is_some_and(|draft| {
            let shown: Vec<usize> = draft
                .visible_rows()
                .iter()
                .filter_map(|m| match draft.rows().get(m.row) {
                    Some(EditRow::Item { item, .. }) => Some(*item),
                    _ => None,
                })
                .collect();
            let Some(field) = draft.fields.iter_mut().find(|f| f.key == "values") else {
                return false;
            };
            let FieldKind::OrderedList { items, .. } = &mut field.kind else {
                return false;
            };
            let mut changed = false;
            for (i, item) in items.iter_mut().enumerate() {
                let want = if tick_all { shown.contains(&i) || item.included } else { false };
                if item.included != want {
                    item.included = want;
                    changed = true;
                }
            }
            changed
        });
        if changed {
            revalidate(shell);
            commit_change(shell, cx);
        } else {
            set_notice(shell, "nothing to change".to_string());
        }
        cx.notify();
        return true;
    }
   ```
   (`draft_ref` is a new one-line sibling of `draft_mut` returning `Option<&Draft>`.) This must sit ABOVE the `filtering` branch's `return false` too? No — in filter mode the `Input` holds `ctrl+a`; the reclaim (`dialog::init_reclaimed_keybindings`) makes both chords reach `handle_key` in both modes exactly as they reach the picker. Place the block once, before the `filtering` branch, so both modes share it.
3. `MoveItem`: add an arm ahead of the generic one —
   ```rust
        NormalCommand::MoveItem(_) if is_scopes(shell) => set_notice(shell, scopes::NO_ORDER_NOTICE.to_string()),
   ```
   with `fn is_scopes(shell: &ShellView) -> bool` reading `state.domain == Domain::Scopes`.
4. `Verb('x')` on Scopes: `Draft::remove_selected`'s available-row refusal says "not in the view — space adds it"; add before the generic `x` arm:
   ```rust
        NormalCommand::Verb('x') if is_scopes(shell) => match draft_ref(shell).map(Draft::selected_row) {
            Some(Some(EditRow::Available { .. })) => set_notice(shell, "not selected — enter picks its values".to_string()),
            Some(Some(EditRow::Item { .. })) if !in_values_stage(shell) => match draft_mut(shell).map(Draft::remove_selected) {
                Some(Step::Changed) => { revalidate(shell); commit_change(shell, cx); }
                Some(Step::Refused(reason)) => set_notice(shell, reason),
                _ => {}
            },
            _ => set_notice(shell, "x drops a selected dimension — here, space unticks".to_string()),
        },
   ```
   (`in_values_stage` mirrors `in_column_stage` off `draft.values()`.)
5. `step_selected_row`: at the top, before toggling —
   ```rust
    // Scopes' edit stage (spec §3): `space` on an available dimension
    // opens its values rather than adding an empty selection; on a
    // selected one it names the door.
    if !in_values_stage(shell) && is_scopes(shell) {
        match draft_ref(shell).and_then(Draft::selected_row) {
            Some(EditRow::Available { .. }) => {
                if let Some(column) = values_stage_target(shell) {
                    enter_values_stage(shell, &column, cx);
                }
                return;
            }
            Some(EditRow::Item { .. }) => {
                set_notice(shell, "enter opens this dimension's values".to_string());
                return;
            }
            _ => {}
        }
    }
   ```
   `on_tick_clicked` walks `step_selected_row`, so the tick on an available Scopes row opens the stage too.
6. `d`/`r`/`o`/`n` in the Values stage: `arm_delete`/`arm_revert`/`overwrite_scope` already refuse inside a column stage via `in_column_stage`; extend each guard to `in_column_stage(shell) || in_values_stage(shell)` with the notice `not a verb while picking values — escape first`. `actions()` returns an empty bar while `draft.values().is_some()` for the same reason it does for the column stage (add `|| draft.values().is_some()` to `in_column`).

- [ ] **Step 5: Crumb, footer, help, painting**

- `crumb_text`: add `Stage::Values { object, column } => format!("{object} › {column}")`.
- `Domain::help`: `if matches!(stage, Stage::Values { .. }) { return scopes::help("values"); }` ahead of the column check.
- `build_edit`'s footer: in the writable normal-mode branch, `let reorders = vocabulary == RowVocabulary::Item && state.domain != Domain::Scopes;`; add after the `types` block:
  ```rust
        if state.domain == Domain::Scopes && draft.values().is_some() {
            hints.push(Hint::new(HintRow::Edit, &["ctrl+a", "ctrl+x"], "all shown / none"));
        }
        if state.domain == Domain::Scopes && draft.values().is_none() && values_stage_target(shell).is_some() {
            hints.push(Hint::new(HintRow::Go, &["enter"], "open values").selector("objectdialog-hint-enter"));
        }
  ```
  and make `leave(..)`'s text `format!("back to {}", draft.name)` when `draft.values().is_some()`, as the column stage does. `opens_column` already gates the column `enter` chip; keep both chips from painting at once (they cannot: `column_stage_target` is `None` on Scopes).
- The item row painter (`build_edit`, the `EditRow::Item`/`Available` arm): replace the `if own { column_summary }` block with
  ```rust
                let note = entry.note.clone().or_else(|| {
                    own.then(|| views::column_summary(&views::kind_default(entry), &entry.presentation))
                        .filter(|s| !s.is_empty())
                });
                if let Some(note) = note {
                    name_row = name_row.child(div().text_xs().text_color(theme.muted_foreground).child(note));
                }
  ```
  The grip is withdrawn on Scopes rows (there is no reorder): where `grip_and_tick` is built, pass `draggable = state.domain != Domain::Scopes` into the existing grip condition (find the `on_drag` site and gate it the same way; `Draft::row_drag` may also answer `None` for Scopes — add `if self.values.is_some() { return None; }` there is NOT enough since the edit stage's own list must not drag either; gate by domain at the render site).
- `section_header_text` for the Values stage: the `values` list is `own` with the Scopes arm's `"DIMENSIONS — …"` text; add a `Stage`-aware override at the call site: when `draft.values().is_some()`, the header reads `"VALUES — space ticks · ctrl+a all shown · ctrl+x none"`, selector `members`.

- [ ] **Step 6: Run the window tests and the crate**

Run: `cargo test -p geode-shell --features test-support objectdialog` then `cargo test -p geode-shell --features test-support`
Expected: PASS, including Task 3's `deliver_values_routes_by_key_and_drops_stale_outcomes`. Then `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt`.

- [ ] **Step 7: Commit**

```bash
git add crates/geode-shell/src/shell
git commit -m "scopes dialog: the Values stage — enter/space open it, distinct request, ticks write, ctrl+a/x, no reorder

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 5: `n` creates empty, `c` duplicates

**Files:**
- Modify: `crates/geode-shell/src/shell/objectdialog/mod.rs` (`NameSeed`, `ObjectDialogState.naming_seed`, `Domain::duplicable`)
- Modify: `crates/geode-shell/src/shell/objectdialog/render.rs` (`create_from_name`, `handle_browse_key`'s `c` arm, `begin_new_object`, `browse_action_bar`, browse footer, naming label)
- Test: `crates/geode-shell/src/shell/tests/objectdialog.rs`

**Interfaces:**
- Produces:
  ```rust
  pub enum NameSeed { Empty, CopyOf(String) }
  pub naming_seed: NameSeed   // on ObjectDialogState, reset to Empty by cancel_naming
  impl Domain { pub fn duplicable(self) -> bool; }   // Scopes alone for now
  ```

- [ ] **Step 1: Write the failing window tests**

Replace `n_on_a_scope_saves_the_frames_current_scope` with:

```rust
/// `n` creates an EMPTY scope (spec §6) — the frame's scope is no longer
/// copied — and opens it with the `new` badge; `o` still takes the
/// frame's.
#[gpui::test]
fn n_on_scopes_creates_an_empty_scope(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_saved_scope(),
        dir.path(),
        "config::scopes",
    );
    shell.update(&mut cx, |shell, cx| {
        shell.frame.update(cx, |f, _| {
            f.set_scope(Scope {
                dimensions: vec![DimensionSelection {
                    column: "book".to_string(),
                    values: vec!["BK007".to_string()],
                }],
                ..Scope::default()
            });
        });
    });
    cx.simulate_keystrokes("n");
    cx.simulate_input("today");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let written = std::fs::read_to_string(dir.path().join("scopes.toml")).unwrap();
    assert!(written.contains("[today"), "{written}");
    assert!(!written.contains("BK007"), "the frame's scope is not copied: {written}");
    assert!(edit_draft(&shell, &cx, |d| d.is_new));
    assert!(edit_draft(&shell, &cx, |d| d.list_items("dimensions").unwrap().is_empty()));
}

/// `c` on a browse row copies that scope verbatim under the typed name
/// and opens the copy.
#[gpui::test]
fn c_duplicates_the_selected_scope_under_a_new_name(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_saved_scope(),
        dir.path(),
        "config::scopes",
    );
    cx.simulate_keystrokes("c");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Naming
    );
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.naming_seed.clone()),
        objectdialog::NameSeed::CopyOf("mine".to_string())
    );
    cx.simulate_input("mine2");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let written = std::fs::read_to_string(dir.path().join("scopes.toml")).unwrap();
    assert!(written.contains("[mine2.dimensions]") && written.contains("BK001"), "{written}");
    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit { ref object } if object == "mine2"
    ));
    assert!(edit_draft(&shell, &cx, |d| d.is_new));
}

/// `c` is Scopes-only for now: elsewhere it is an unbound letter.
#[gpui::test]
fn c_is_not_a_verb_on_views(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx,
        services_with_a_desk_view(),
        dir.path(),
        "config::views",
    );
    cx.simulate_keystrokes("c");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Browse
    );
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-shell --features test-support n_on_scopes_creates c_duplicates`
Expected: FAIL (`n` still copies the frame; `naming_seed` missing).

- [ ] **Step 3: Implement**

`mod.rs`:
```rust
/// What `enter` in [`Stage::Naming`] creates (scopes-editing spec §6):
/// the domain's empty object (`n`), or a verbatim copy of a named one
/// (`c`). Recorded by NAME when armed — the browse cursor is an index,
/// and a reload can re-rank the list under it (the same reason
/// `confirm_target` records one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameSeed {
    Empty,
    CopyOf(String),
}
```
`ObjectDialogState` gains `pub naming_seed: NameSeed` (`NameSeed::Empty` in `new`, reset in `cancel_naming`). `Domain` gains:
```rust
    /// May `c` copy an object under a new name? Scopes alone for now
    /// (scopes-editing spec §6); the mechanism is generic.
    pub fn duplicable(self) -> bool {
        self == Domain::Scopes
    }
```

`render.rs` `handle_browse_key`: beside the `Verb('n')` arm —
```rust
            NormalCommand::Verb('c') if state.domain.duplicable() => {
                let name = selected_row(state, &rows, &visible).map(|r| r.name.clone());
                match name {
                    Some(name) => begin_copy(shell, name),
                    None => set_notice(shell, "nothing selected to copy".to_string()),
                }
                cx.notify();
                return true;
            }
```
(`rows`/`visible` are already derived in that function for `d`/`r`; reuse them.) With:
```rust
/// `c` (scopes-editing spec §6): the naming stage seeded to copy
/// `source` — `begin_new_object`'s twin, without Sources' dataset seed.
fn begin_copy(shell: &mut ShellView, source: String) {
    let Some(state) = shell.object_dialog.as_mut() else { return; };
    if !state.domain.writable(&state.stage) {
        state.notice = Some(READ_ONLY_NOTICE.to_string());
        return;
    }
    state.begin_naming();
    state.naming_seed = NameSeed::CopyOf(source);
}
```
`begin_new_object` sets `state.naming_seed = NameSeed::Empty` after `begin_naming()`.

`create_from_name`: replace the `if domain == Domain::Scopes { … frame … }` block with
```rust
    let seed = shell
        .object_dialog
        .as_ref()
        .map(|s| s.naming_seed.clone())
        .unwrap_or(NameSeed::Empty);
    if let NameSeed::CopyOf(source) = seed {
        // Verbatim, from the pending-aware config (§6): inside the write
        // debounce `services.config` is the source as it stood before
        // its last edit.
        let folded = apply::config_with_pending(shell);
        let config = folded.as_ref().unwrap_or(&shell.services.config);
        let Some(table) = config
            .doc(domain.doc())
            .and_then(|doc| doc.value.get(&source))
            .and_then(|v| v.as_table())
            .cloned()
        else {
            set_notice(shell, format!("'{source}' is gone — nothing to copy"));
            cx.notify();
            return;
        };
        draft.source = table;
        draft.fields = domain.fields_from_source(config, &draft.source);
        draft.diagnostics = domain.validate(&draft, config);
    }
```
`Domain::fields_from_source(config, &toml::Table) -> Vec<Field>` is a new match in `mod.rs` that, for Scopes, calls `scopes::fields_from_table(config, Some(table))` (make it `pub(super)`) and for every other domain falls back to `self.fields(config, None)` — documented as "only a duplicable domain needs the real answer". Scopes' `new_draft` path (empty) needs no special case now: `fields(config, None)` already builds the empty list with every pickable column available, and `Draft::new_object`'s empty source renders `{}` — make `scopes::to_table` render the three keys explicitly when `source` lacks them (`dimensions = {}`, `text = ""`, `expression = ""`) so the file shows the shape (`spec §6`): in `to_table`, clone `source`, `entry(..).or_insert` the three, then convert.

Naming label: where the name row's label is built (`"New {} · name"`), read `naming_seed` — `NameSeed::CopyOf(src)` paints `format!("Copy of {src} · name")`.

`browse_action_bar`: an `offers_c = !naming && writable && state.domain.duplicable() && row.is_some()` button (`objectdialog-action-c`, label `Copy this scope`) whose click runs the same `begin_copy` with the selected row's name, then `sync_dialog_text`. Browse footer: `Hint::new(HintRow::Edit, &["c"], "copy")` beside `n` when `duplicable()`.

`selected_row` in `handle_browse_key`'s `c` arm: use the existing `selected_row(state, &rows, &visible)` helper (line ~2293) — check its signature and pass what it takes.

- [ ] **Step 4: Run tests, clippy, fmt**

Run: `cargo test -p geode-shell --features test-support` and `cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS. `n_is_inert_on_groupings_and_says_why` unchanged.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-shell/src/shell
git commit -m "scopes dialog: n creates an empty scope, c copies the selected one

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 6: Mutation entries, docs, hand-off

**Files:**
- Modify: `scripts/mutation-check.sh`
- Modify: `docs/superpowers/specs/2026-09-08-geode-phase-4c-config-dialogs-design.md` (new §23)
- Modify: `CLAUDE.md` (the Part 2a paragraph's Scopes sentences)

- [ ] **Step 1: Harness entries** (append after the existing `scopes::overwrite_with` entries, each naming its test)

```bash
run_mutation "scopes dialog: deliver_distinct routes SCOPES_KEY to the dialog" \
  crates/geode-shell/src/shell/mod.rs \
  '        if outcome.key == SCOPES_KEY {' \
  '        if false {' \
  geode-shell deliver_values_routes_by_key_and_drops_stale_outcomes

run_mutation "scopes dialog: a stale values tag is dropped" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '    if *column != outcome.column || outcome.tag != state.values_tag {' \
  '    if *column != outcome.column {' \
  geode-shell deliver_values_routes_by_key_and_drops_stale_outcomes

run_mutation "scopes dialog: a saved value the data lacks stays ticked and marked" \
  crates/geode-shell/src/shell/objectdialog/scopes.rs \
  '        if !values.iter().any(|(v, _)| v == value) {' \
  '        if false {' \
  geode-shell values_fields_tick_the_saved_ones_and_keep_a_stale_one_marked

run_mutation "scopes dialog: an emptied selection removes the key, never []" \
  crates/geode-shell/src/shell/objectdialog/scopes.rs \
  '        if ticked.is_empty() {' \
  '        if false {' \
  geode-shell fold_values_writes_the_ticks_and_removes_an_emptied_selection

run_mutation "scopes dialog: a broken expression is refused" \
  crates/geode-shell/src/shell/objectdialog/scopes.rs \
  '    if key == "expression" && !text.is_empty() {' \
  '    if false {' \
  geode-shell text_and_expression_are_editable_and_the_expression_is_parsed

run_mutation "scopes dialog: reorder is refused" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '        NormalCommand::MoveItem(_) if is_scopes(shell) => set_notice(shell, scopes::NO_ORDER_NOTICE.to_string()),' \
  '        NormalCommand::MoveItem(_) if false => set_notice(shell, scopes::NO_ORDER_NOTICE.to_string()),' \
  geode-shell ctrl_a_and_ctrl_x_tick_and_clear_and_reorder_is_refused

run_mutation "scopes dialog: c copies verbatim" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '        draft.source = table;' \
  '        let _ = table;' \
  geode-shell c_duplicates_the_selected_scope_under_a_new_name

run_mutation "scopes dialog: the request carries the draft minus the column" \
  crates/geode-shell/src/shell/objectdialog/scopes.rs \
  '    scope.dimensions.retain(|d| d.column != minus);' \
  '    let _ = minus;' \
  geode-shell entering_the_values_stage_requests_the_columns_distinct_values
```

Run: `zsh scripts/mutation-check.sh --anchors-only` (must exit 0), then `zsh scripts/mutation-check.sh "scopes dialog:"`. Every entry must report CAUGHT; fix any anchor that matches twice.

- [ ] **Step 2: Docs**

4c spec: append `## 23. Scopes editing (2026-09-19)` — three sentences pointing at `2026-09-19-geode-scopes-dialog-editing-design.md`, stating §8.4's "values are not edited here" is superseded, and listing the keys (`enter`/`space` open values, `ctrl+a`/`ctrl+x`, `i` on text/expression, `n` empty, `c` copy, `o` unchanged).

CLAUDE.md: in the "Phase 4c Part 2a is done" paragraph, replace the sentence beginning "`Domain::Scopes` (`shell::objectdialog::scopes`) is the thinnest adapter:" through "…nothing to confirm." with: "`Domain::Scopes` (`shell::objectdialog::scopes`) edits a scope's own criteria since 2026-09-19 (spec `2026-09-19-geode-scopes-dialog-editing-design.md`, reversing 4c §8.4): `dimensions` is an `OrderedList` (selected columns as items with a values note, the other `pickable_columns` available), `text` and `expression` are `i`-editable (`parse_text` refuses a broken expression with `parse_expr`'s message), and `enter`/`space` on a dimension row opens `Stage::Values` — a projection like `Stage::Column` (`Draft.values` beside `Draft.column`, `enter_values`/`leave_values`) whose one list is the column's distinct values fetched through `ShellEvent::DistinctRequested` under `SCOPES_KEY` (`deliver_distinct` routes on `outcome.key`; `ObjectDialogState.values_tag` is the staleness check) with a saved value the data lacks painted ticked and noted `not in data`. Every mutation folds into `draft.source` (`scopes::fold`, `scopes::fold_values` from `revalidate`) so `to_table` still renders `source` and `is_dirty` still compares it; an emptied selection removes the key rather than writing `[]`; reorder is refused (`NO_ORDER_NOTICE`). `n` creates an empty scope, `c` (`NameSeed::CopyOf`, Scopes-only via `Domain::duplicable`) copies the selected one, `o` still takes the frame's scope. **Two traps:** `fold` must not retain `dimensions` while the Values stage is open (the `dimensions` field is stashed, so `kept` would be empty and every selection vanish), and `step_selected`'s keep-one-entry guard stands down only under `Draft.values` — never by `dest`."

- [ ] **Step 3: Final verification and commit**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo check -p geode-shell --features test-support --all-targets && zsh scripts/mutation-check.sh --anchors-only`
Expected: all green.

```bash
git add scripts/mutation-check.sh docs CLAUDE.md
git commit -m "docs: scopes dialog editing — 4c §23, CLAUDE.md, mutation entries

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

Then request a whole-branch review (superpowers:requesting-code-review) before merging; display checks (the values list's notes, the `not in data` row, the loading row, the copy label in the naming row) are pending on a real window as every dialog's are.
