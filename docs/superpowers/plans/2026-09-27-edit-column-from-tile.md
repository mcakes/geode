# Edit a Tile's Column in Views or Schema — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Two palette actions, "Edit column in view…" and "Edit column in schema…", open a column list from the focused tile's columns (cursor column preselected) and land the Views or Schema object dialog on that column's Column stage.

**Architecture:** The tile reports its view and presented column names through a new pull method `TileContent::tile_columns` (vocabulary type in `geode-core`). The shell opens a `choicedialog` list over them; a commit closes the list and runs a new `objectdialog::render::open_column`, which resolves the dialog object (the view, or the owning dataset via `DatasetPresentationSpec::owner_of`) against pending-aware config and chains `open` → `enter_edit_stage` → `enter_column_stage`, reporting failures in the dialog's footer notice.

**Tech Stack:** Rust, GPUI, gpui-component; crates `geode-core`, `geode-shell`, `geode-blotter`.

**Spec:** `docs/superpowers/specs/2026-09-27-edit-column-from-tile-design.md`

## Global Constraints

- Palette only: no `:` command, no default key binding (spec ruling 1).
- Action ids `config::view_column` / `config::schema_column`, titles `Edit column in view…` / `Edit column in schema…` (U+2026), category `Configuration`.
- Every non-tree column the tile presents qualifies; the Schema list omits `derived` columns.
- Column identity is the dataset/view column `name`, never the label.
- Resolution reads `apply::config_with_pending(shell)`, falling back to `shell.services.config`; never the tile's factory copy.
- User-facing text says "color", not "colour".
- Shell status notices are `&'static str`; formatted notices go through the object dialog's `set_notice`.
- `geode-shell` and `geode-blotter` never depend on each other beyond the existing blotter → shell edge; `geode-core` stays I/O-free.
- Commit messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- After each task: `cargo fmt --check` and `cargo clippy -p <crate> --all-targets -- -D warnings` for touched crates.

## Review Focus

1. Two columns with the same painted label (e.g. both unlabeled `npv` from different joins cannot happen, but two labels `P&L`): row text includes the name when label ≠ name, so rows stay distinct — Task 4 test `column_rows_name_the_column_when_the_label_differs`.
2. Cursor on the tree column: nothing is preselected, list opens on its first row — Task 2 test `the_tree_column_is_no_active_column` and Task 4 test `no_active_column_places_the_first_row`.
3. The Views dialog already open on top of the stack when the action runs (silent refusal) and covered under Colors (notice) — Task 4 GPUI test `a_covered_views_dialog_refuses_before_the_list_opens`.
4. The view the tile names no longer exists at commit — Task 4 GPUI test `an_undefined_view_lands_in_browse_with_a_notice`.
5. A filter that leaves no row lit: Enter keeps the list open and commits nothing (existing `handle_key` behaviour; the new target must not bypass it) — Task 4 GPUI test `enter_with_every_row_filtered_out_commits_nothing`.

---

## File Structure

| File | Change | Responsibility |
|---|---|---|
| `crates/geode-core/src/tile_columns.rs` | Create | `TileColumns`, `TileColumn` vocabulary |
| `crates/geode-core/src/lib.rs` | Modify | `pub mod tile_columns;` |
| `crates/geode-shell/src/module.rs` | Modify | `TileContent::tile_columns` default; recording fixture field |
| `crates/geode-blotter/src/core/plan.rs` | Modify | `ColumnPlan::tile_columns` (pure) |
| `crates/geode-blotter/src/tile.rs`, `content.rs` | Modify | wire the trait method |
| `crates/geode-shell/src/shell/objectdialog/render.rs` | Modify | `resolve_column_object` (pure), `open_column`, `enter_column_stage -> bool` |
| `crates/geode-shell/src/shell/choicedialog.rs` | Modify | `Target::Column`, `Pick::Column`, `open_columns`, commit |
| `crates/geode-shell/src/defaults.rs` | Modify | register the two actions |
| `crates/geode-shell/src/shell/input.rs` | Modify | dispatch arms |
| `crates/geode-shell/src/shell/dialog.rs` | Modify | `opens_dialog` lists both ids |
| `crates/geode-shell/src/shell/tests/edit_column.rs` | Create | GPUI tests through the palette route |
| `crates/geode-shell/src/shell/tests/mod.rs`, `dialog_stack.rs` | Modify | register test module; `REFUSES_IN_FIXTURE` |
| `scripts/mutation-check.sh` | Modify | targeted entries |
| docs (Task 5) | Modify | current guides, READMEs, TODO.md |

---

### Task 1: `TileColumns` vocabulary and the `TileContent` pull method

**Files:**
- Create: `crates/geode-core/src/tile_columns.rs`
- Modify: `crates/geode-core/src/lib.rs` (after `pub mod launch;`)
- Modify: `crates/geode-shell/src/module.rs` (trait, near `launch_context` at ~252; recording fixture near 645, 671, 730, 895, 1006)

**Interfaces:**
- Produces: `geode_core::tile_columns::{TileColumns, TileColumn}`; `TileContent::tile_columns(&self, cx: &App) -> Option<TileColumns>`; `RecordingFactory.tile_columns: Rc<RefCell<Option<TileColumns>>>` (shared into each `RecordingView`/content like `launch_context`).

- [ ] **Step 1: Write the vocabulary with its test**

`crates/geode-core/src/tile_columns.rs`:

```rust
//! The columns a tile presents from a named view, and the one at its cursor.
//! The shell reads the focused tile's `TileContent::tile_columns` to offer
//! a column to edit in the Views or Schema dialog; the tile reports names
//! only and the shell resolves them against current configuration, so a
//! tile never needs the dialog's vocabulary.

/// A tile's presented view columns, in display order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TileColumns {
    /// The view the tile shows, by name.
    pub view: String,
    pub columns: Vec<TileColumn>,
    /// Index into `columns`; `None` when the cursor is on no listed column
    /// (a tree column, or no cursor yet).
    pub active: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileColumn {
    /// The view/dataset column name — the identity every lookup uses.
    pub name: String,
    /// The header label as painted.
    pub label: String,
    /// A view-derived column: no dataset declares it, so only Views edits it.
    pub derived: bool,
}

impl TileColumns {
    /// The column at the cursor, if any.
    pub fn active_column(&self) -> Option<&TileColumn> {
        self.active.and_then(|ix| self.columns.get(ix))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(name: &str) -> TileColumn {
        TileColumn {
            name: name.into(),
            label: name.into(),
            derived: false,
        }
    }

    #[test]
    fn the_active_column_is_the_indexed_one_or_none() {
        let mut t = TileColumns {
            view: "tree".into(),
            columns: vec![col("npv"), col("delta01")],
            active: Some(1),
        };
        assert_eq!(t.active_column().map(|c| c.name.as_str()), Some("delta01"));
        t.active = None;
        assert_eq!(t.active_column(), None);
        t.active = Some(9);
        assert_eq!(t.active_column(), None, "an out-of-range index names nothing");
    }
}
```

Add `pub mod tile_columns;` after `pub mod launch;` in `crates/geode-core/src/lib.rs`.

- [ ] **Step 2: Run the test**

Run: `cargo test -p geode-core tile_columns`
Expected: PASS (1 test).

- [ ] **Step 3: Add the trait method**

In `crates/geode-shell/src/module.rs`, add `use geode_core::tile_columns::TileColumns;` beside the `launch` import, and after `fn launch_context` in `TileContent`:

```rust
    /// The view columns this tile presents and the one at its cursor, pulled
    /// by the shell when `config::view_column` / `config::schema_column`
    /// runs, so the module needs no handle into the shell. `None` (the
    /// default) for a tile that shows no configured view; the shell then
    /// refuses with a status notice.
    fn tile_columns(&self, _cx: &App) -> Option<TileColumns> {
        None
    }
```

- [ ] **Step 4: Extend the recording fixture**

In the `recording` module of `module.rs`, mirror `launch_context` exactly:

- `RecordingFactory` gains
  ```rust
          /// What every occupant this factory creates answers from
          /// `tile_columns`; shared and mutable like `launch_context`.
          pub tile_columns: Rc<RefCell<Option<TileColumns>>>,
  ```
  initialised `tile_columns: Rc::new(RefCell::new(None)),` in `RecordingFactory::new`.
- The occupant struct holding `launch_context: Rc<RefCell<LaunchContext>>` (~730) gains `tile_columns: Rc<RefCell<Option<TileColumns>>>`, set from the factory where `launch_context: self.launch_context.clone(),` is (~1006).
- Its `TileContent` impl (~895) gains
  ```rust
          fn tile_columns(&self, _cx: &App) -> Option<TileColumns> {
              self.tile_columns.borrow().clone()
          }
  ```

- [ ] **Step 5: Build**

Run: `cargo check -p geode-shell --all-targets && cargo check -p geode-shell --features test-support --all-targets`
Expected: both succeed.

- [ ] **Step 6: Commit**

```bash
git add crates/geode-core/src/tile_columns.rs crates/geode-core/src/lib.rs crates/geode-shell/src/module.rs
git commit -m "feat(core): TileColumns vocabulary and TileContent::tile_columns

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: The blotter answers `tile_columns`

**Files:**
- Modify: `crates/geode-blotter/src/core/plan.rs` (new method on `ColumnPlan`; tests in its `mod tests`)
- Modify: `crates/geode-blotter/src/tile.rs` (beside `launch_context` at ~438)
- Modify: `crates/geode-blotter/src/content.rs` (beside `fn launch_context` at ~175)

**Interfaces:**
- Consumes: `TileColumns`, `TileColumn` (Task 1); `TileContent::tile_columns`.
- Produces: `ColumnPlan::tile_columns(&self, view: &ViewSpec, cursor_col: usize) -> TileColumns`; `BlotterTile::tile_columns(&self, cx: &App) -> Option<TileColumns>`.

- [ ] **Step 1: Write the failing tests** in `plan.rs`'s `mod tests` (the fixture `view()` has grouping `["lhu", "underlying_ref"]`, so `lhu` folds into the tree; the plan's columns are tree, `model_code`, `delta01` (label `Δ (k)`), `daily_trading_pnl`, `missing_in_snapshot`):

```rust
    #[test]
    fn tile_columns_lists_the_non_tree_columns_in_display_order() {
        let snap = snapshot();
        let plan = ColumnPlan::build(&view(), snap.grouping(), &snap);
        let t = plan.tile_columns(&view(), 2);
        assert_eq!(t.view, "tree");
        let names: Vec<&str> = t.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            ["model_code", "delta01", "daily_trading_pnl", "missing_in_snapshot"]
        );
        assert_eq!(t.columns[1].label, "Δ (k)", "the label as painted");
        // Plan index 2 is `delta01`; the context drops the tree column, so 1.
        assert_eq!(t.active, Some(1));
        assert!(t.columns.iter().all(|c| !c.derived));
    }

    #[test]
    fn the_tree_column_is_no_active_column() {
        let snap = snapshot();
        let plan = ColumnPlan::build(&view(), snap.grouping(), &snap);
        assert_eq!(plan.tile_columns(&view(), 0).active, None);
        assert_eq!(plan.tile_columns(&view(), 99).active, None);
    }

    #[test]
    fn a_derived_view_column_is_flagged() {
        let text = r#"
[tree]
dataset = "risk_snapshot"
grouping = ["lhu"]
[[tree.columns]]
name = "delta01"
[[tree.columns]]
name = "delta_x2"
kind = "derived"
sql = "delta01 * 2"
"#;
        let doc = merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()]);
        let v = ViewSpec::from_doc(&doc).0.remove(0);
        let snap = snapshot();
        let plan = ColumnPlan::build(&v, &["lhu".to_string()], &snap);
        let t = plan.tile_columns(&v, 1);
        let flags: Vec<(&str, bool)> = t
            .columns
            .iter()
            .map(|c| (c.name.as_str(), c.derived))
            .collect();
        assert_eq!(flags, [("delta01", false), ("delta_x2", true)]);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p geode-blotter tile_columns derived_view_column tree_column_is_no_active`
Expected: FAIL — `no method named tile_columns found for struct ColumnPlan`.

- [ ] **Step 3: Implement** in `impl ColumnPlan` (plan.rs), adding `use geode_core::tile_columns::{TileColumn, TileColumns};`:

```rust
    /// The presented columns as the shell's column list names them: every
    /// non-tree column in display order, `derived` from the view definition,
    /// and `active` the plan column at `cursor_col` unless that is the tree
    /// column (index 0) or out of range.
    pub fn tile_columns(&self, view: &ViewSpec, cursor_col: usize) -> TileColumns {
        let columns = self
            .columns
            .iter()
            .filter(|c| c.kind != ColumnKind::Tree)
            .map(|c| TileColumn {
                name: c.name.clone(),
                label: c.label.clone(),
                derived: view.columns.iter().any(|v| {
                    v.name() == c.name && matches!(v, ViewColumn::Derived { .. })
                }),
            })
            .collect();
        let active = (cursor_col > 0 && cursor_col < self.columns.len()).then(|| cursor_col - 1);
        TileColumns {
            view: view.name.clone(),
            columns,
            active,
        }
    }
```

(The tree column is always plan index 0 — `ColumnPlan::build` pushes it first — so plan index `i > 0` is context index `i - 1`.)

- [ ] **Step 4: Run the tests**

Run: `cargo test -p geode-blotter plan::`
Expected: PASS.

- [ ] **Step 5: Wire the tile and the content**

`tile.rs`, after `launch_context`:

```rust
    /// The presented columns and the cursor's column, for the shell's
    /// edit-column actions. `None` until a plan exists or when the view is
    /// no longer configured.
    pub fn tile_columns(&self, cx: &App) -> Option<geode_core::tile_columns::TileColumns> {
        let view = self.view()?;
        let d = self.table.read(cx).delegate();
        d.plan
            .as_ref()
            .map(|plan| plan.tile_columns(&view, d.cursor.col))
    }
```

`content.rs`, in `impl TileContent`, after `launch_context`:

```rust
    fn tile_columns(&self, cx: &App) -> Option<geode_core::tile_columns::TileColumns> {
        self.tile.read(cx).tile_columns(cx)
    }
```

- [ ] **Step 6: Build and test**

Run: `cargo test -p geode-blotter && cargo clippy -p geode-blotter --all-targets -- -D warnings`
Expected: PASS, no warnings.

- [ ] **Step 7: Commit**

```bash
git add crates/geode-blotter
git commit -m "feat(blotter): report presented columns and the cursor column

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: `open_column` — the object-dialog route to one column

**Files:**
- Modify: `crates/geode-shell/src/shell/objectdialog/render.rs` (beside `open_object` at ~697; `enter_column_stage` at ~862; its callers at ~1609 and ~4727)
- Test: `render.rs`'s existing `#[cfg(test)]` module if it has one, else a new `#[cfg(test)] mod column_route_tests` at the end of `render.rs`

**Interfaces:**
- Consumes: `Domain`, `apply::config_with_pending`, `enter_edit_stage`, `set_notice`, `geode_core::config::load_views`, `geode_core::schema::SchemaSpec`, `geode_core::view::DatasetPresentationSpec::owner_of`.
- Produces:
  - `fn resolve_column_object(domain: Domain, views: &[ViewSpec], schema: &SchemaSpec, view: &str, column: &str) -> Result<String, String>` (private, pure)
  - `pub(in crate::shell) fn open_column(shell: &mut ShellView, domain: Domain, view: &str, column: &str, window: &mut Window, cx: &mut Context<ShellView>)`
  - `fn enter_column_stage(...) -> bool` (was `()`; `true` only when the stage became `Column`)

- [ ] **Step 1: Write the failing pure tests**

```rust
#[cfg(test)]
mod column_route_tests {
    use super::*;
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::schema::SchemaSpec;
    use geode_core::view::{JoinSpec, ViewColumn, ViewSpec};

    fn schema() -> SchemaSpec {
        let text = "[risk_snapshot.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                    [risk_snapshot.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n\
                    [instrument_ref.columns.sector]\ntype = \"utf8\"\nrole = \"dimension\"\n";
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        SchemaSpec::from_doc(&doc).0
    }

    fn views() -> Vec<ViewSpec> {
        vec![ViewSpec {
            name: "tree".into(),
            dataset: "risk_snapshot".into(),
            joins: vec![JoinSpec {
                dataset: "instrument_ref".into(),
                on: vec!["instrument_ref".into()],
                required: true,
            }],
            columns: vec![
                ViewColumn::Measure { name: "npv".into(), required: true },
                ViewColumn::Dimension { name: "sector".into(), required: true },
                ViewColumn::Derived { name: "npv_x2".into(), sql: "npv * 2".into(), required: true },
            ],
            ..ViewSpec::default()
        }]
    }

    #[test]
    fn views_resolves_to_the_view_itself() {
        assert_eq!(
            resolve_column_object(Domain::Views, &views(), &schema(), "tree", "npv"),
            Ok("tree".to_string())
        );
    }

    #[test]
    fn schema_resolves_a_primary_column_to_the_view_dataset() {
        assert_eq!(
            resolve_column_object(Domain::Schema, &views(), &schema(), "tree", "npv"),
            Ok("risk_snapshot".to_string())
        );
    }

    #[test]
    fn schema_resolves_a_joined_column_to_its_owning_dataset() {
        assert_eq!(
            resolve_column_object(Domain::Schema, &views(), &schema(), "tree", "sector"),
            Ok("instrument_ref".to_string())
        );
    }

    #[test]
    fn schema_refuses_a_column_no_dataset_declares() {
        assert_eq!(
            resolve_column_object(Domain::Schema, &views(), &schema(), "tree", "npv_x2"),
            Err("'npv_x2' is not declared by any dataset of view 'tree'".to_string())
        );
    }

    #[test]
    fn an_undefined_view_is_refused_for_both_domains() {
        for domain in [Domain::Views, Domain::Schema] {
            assert_eq!(
                resolve_column_object(domain, &views(), &schema(), "gone", "npv"),
                Err("view 'gone' is not defined".to_string())
            );
        }
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p geode-shell column_route_tests`
Expected: FAIL — `cannot find function resolve_column_object`.

- [ ] **Step 3: Implement the pure resolver** (above `open_object`):

```rust
/// The object an edit-column route opens for `column` of `view`: the view itself
/// for Views; for Schema, the first dataset of the view (primary, then joins) that
/// declares the column — the same owner `view_presentation` resolves. `Err` is the
/// footer notice.
fn resolve_column_object(
    domain: Domain,
    views: &[ViewSpec],
    schema: &SchemaSpec,
    view: &str,
    column: &str,
) -> Result<String, String> {
    let Some(spec) = views.iter().find(|v| v.name == view) else {
        return Err(format!("view '{view}' is not defined"));
    };
    match domain {
        Domain::Schema => DatasetPresentationSpec::owner_of(spec, column, schema)
            .map(str::to_string)
            .ok_or_else(|| {
                format!("'{column}' is not declared by any dataset of view '{view}'")
            }),
        _ => Ok(view.to_string()),
    }
}
```

Add imports as needed: `geode_core::view::{DatasetPresentationSpec, ViewSpec}`, `geode_core::schema::SchemaSpec`, `geode_core::config::load_views` (check existing `use` lines first; `render.rs` already names `geode_core::schema::SchemaSpec` fully-qualified inside `enter_column_stage`).

- [ ] **Step 4: Run the pure tests**

Run: `cargo test -p geode-shell column_route_tests`
Expected: PASS (5 tests).

- [ ] **Step 5: `enter_column_stage` returns whether it entered**

Change the signature to `fn enter_column_stage(shell: &mut ShellView, column: &str, cx: &mut Context<ShellView>) -> bool`. Every early `return;` becomes `return false;`; after the `if !draft.enter_column(column, fields) { draft.column_ctx = None; return false; }` branch, the success path ends with `cx.notify(); true`. Update the doc comment's last sentence to: "If the column cannot be resolved, discard the prepared context, leave the current stage intact and return `false`." The two existing callers (~1609, ~4727) ignore the result: `let _ = enter_column_stage(shell, &name, cx);` is not needed — a bare call statement of a `bool` is fine unless the function gets `#[must_use]`; do not add one.

- [ ] **Step 6: Implement `open_column`** directly after `open_object`:

```rust
/// Open `domain`'s dialog straight onto `column`'s Column stage for the tile's
/// `view` (`config::view_column` / `config::schema_column`). Resolves the object
/// against the pending-aware config at this moment, not the tile's copy, because
/// a reload or a queued edit may have changed the view since the tile planned.
/// Each failure lands in the dialog's footer notice at the stage it reached:
/// an unresolvable object leaves Browse, a column the object no longer has
/// leaves its Edit stage.
pub(in crate::shell) fn open_column(
    shell: &mut ShellView,
    domain: Domain,
    view: &str,
    column: &str,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    // Same guard as `open_object`: without it the stages below would land on
    // whatever object dialog is live.
    if !dialog::can_open_object(shell, domain) {
        return;
    }
    let object = {
        let folded = apply::config_with_pending(shell);
        let config = folded.as_ref().unwrap_or(&shell.services.config);
        let (views, _) = load_views(config);
        let schema = config
            .doc("datasets")
            .map(|doc| SchemaSpec::from_doc(doc).0)
            .unwrap_or_default();
        resolve_column_object(domain, &views, &schema, view, column)
    };
    open(shell, domain, window, cx);
    match object {
        Err(notice) => set_notice(shell, notice),
        Ok(object) => {
            enter_edit_stage(shell, &object, None, cx);
            if !enter_column_stage(shell, column, cx) {
                set_notice(shell, format!("'{column}' is not a column of '{object}'"));
            }
        }
    }
    // `open` synchronized the shared input for Browse; the stage now on screen
    // needs its own pass.
    dialog::sync_dialog_text(shell, window, cx);
    cx.notify();
}
```

- [ ] **Step 7: Build and test**

Run: `cargo test -p geode-shell objectdialog && cargo clippy -p geode-shell --all-targets -- -D warnings`
Expected: PASS. `open_column` is unused until Task 4; if Clippy/rustc flags `dead_code`, add `#[allow(dead_code)] // wired by the choice dialog in the next commit` and remove it in Task 4.

- [ ] **Step 8: Commit**

```bash
git add crates/geode-shell/src/shell/objectdialog/render.rs
git commit -m "feat(shell): open an object dialog straight onto a column stage

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: The column list and the two palette actions

**Files:**
- Modify: `crates/geode-shell/src/shell/choicedialog.rs`
- Modify: `crates/geode-shell/src/defaults.rs` (after `config::schema` at ~250)
- Modify: `crates/geode-shell/src/shell/input.rs` (after the `config::expressions` arm at ~253)
- Modify: `crates/geode-shell/src/shell/dialog.rs` (`opens_dialog`, ~210)
- Modify: `crates/geode-shell/src/shell/tests/dialog_stack.rs` (`REFUSES_IN_FIXTURE`, ~321)
- Create: `crates/geode-shell/src/shell/tests/edit_column.rs`; add `mod edit_column;` in `tests/mod.rs` (alphabetical, after `mod dock;`/`mod drag;`)

**Interfaces:**
- Consumes: `TileColumns`/`TileColumn` (Task 1), `RecordingFactory.tile_columns` (Task 1), `objectdialog::render::open_column` (Task 3), `dialog::can_open_object`.
- Produces:
  - `Target::Column { domain: Domain, view: String, names: Vec<String> }`
  - `Pick::Column { domain: Domain, view: String, column: String }`
  - `ChoiceDialogState::columns(domain: Domain, tile: &TileColumns) -> Option<Self>`
  - `pub fn open_columns(view: &mut ShellView, domain: Domain, window: &mut Window, cx: &mut Context<ShellView>)`
  - `pub(crate) const NO_TILE_COLUMNS: &str = "this tile has no dataset columns";`
  - `pub(crate) const NO_SCHEMA_COLUMNS: &str = "no schema columns in this tile's view";`

- [ ] **Step 1: Write the failing pure tests** in `choicedialog.rs`'s `#[cfg(test)] mod tests` (create one at the end of the file if absent, with `use super::*;`):

```rust
    fn tile() -> TileColumns {
        let c = |name: &str, label: &str, derived: bool| TileColumn {
            name: name.into(),
            label: label.into(),
            derived,
        };
        TileColumns {
            view: "tree".into(),
            columns: vec![
                c("model_code", "model_code", false),
                c("npv", "NPV", false),
                c("npv_x2", "npv_x2", true),
            ],
            active: Some(1),
        }
    }

    #[test]
    fn column_rows_name_the_column_when_the_label_differs() {
        let s = ChoiceDialogState::columns(Domain::Views, &tile()).unwrap();
        assert_eq!(s.list.options(), ["model_code", "NPV · npv", "npv_x2"]);
    }

    #[test]
    fn the_cursor_column_is_preselected() {
        let s = ChoiceDialogState::columns(Domain::Views, &tile()).unwrap();
        assert_eq!(
            s.highlighted_pick(),
            Some(Pick::Column {
                domain: Domain::Views,
                view: "tree".into(),
                column: "npv".into(),
            })
        );
    }

    #[test]
    fn schema_omits_derived_columns() {
        let s = ChoiceDialogState::columns(Domain::Schema, &tile()).unwrap();
        assert_eq!(s.list.options(), ["model_code", "NPV · npv"]);
    }

    #[test]
    fn a_derived_cursor_column_preselects_nothing_in_schema() {
        let mut t = tile();
        t.active = Some(2);
        let s = ChoiceDialogState::columns(Domain::Schema, &t).unwrap();
        assert_eq!(
            s.highlighted_pick(),
            Some(Pick::Column {
                domain: Domain::Schema,
                view: "tree".into(),
                column: "model_code".into(),
            }),
            "first row"
        );
    }

    #[test]
    fn no_active_column_places_the_first_row() {
        let mut t = tile();
        t.active = None;
        let s = ChoiceDialogState::columns(Domain::Views, &t).unwrap();
        assert!(matches!(
            s.highlighted_pick(),
            Some(Pick::Column { ref column, .. }) if column == "model_code"
        ));
    }

    #[test]
    fn a_schema_list_with_only_derived_columns_is_none() {
        let mut t = tile();
        t.columns.retain(|c| c.derived);
        t.active = None;
        assert_eq!(ChoiceDialogState::columns(Domain::Schema, &t), None);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p geode-shell choicedialog::tests`
Expected: FAIL — `no function or associated item named columns`.

- [ ] **Step 3: Implement the pure core** in `choicedialog.rs`:

Imports: `use geode_core::tile_columns::{TileColumn, TileColumns};` and `use super::objectdialog::{self, Domain};`.

Module doc: append a paragraph:

```rust
//! `config::view_column` / `config::schema_column` list the focused tile's
//! presented columns (Schema without derived ones), the cursor's column
//! highlighted; a pick opens that dialog on the column's Column stage.
```

`Target` gains:

```rust
    /// The focused tile's columns for `config::view_column` (Views) or
    /// `config::schema_column` (Schema), captured at open: `names[i]` is the
    /// column declared option `i` stands for.
    Column {
        domain: Domain,
        view: String,
        names: Vec<String>,
    },
```

`Pick` gains:

```rust
    /// `objectdialog::render::open_column` for this column of `view`.
    Column {
        domain: Domain,
        view: String,
        column: String,
    },
```

Constructor in `impl ChoiceDialogState`:

```rust
    /// The column rows for `tile` (derived columns left out for Schema: no
    /// dataset declares them), the highlight on the cursor's column when it
    /// is listed, else the first. `None` when no row is left.
    pub fn columns(domain: Domain, tile: &TileColumns) -> Option<Self> {
        let mut options = Vec::new();
        let mut names = Vec::new();
        let mut active = None;
        for (ix, c) in tile.columns.iter().enumerate() {
            if domain == Domain::Schema && c.derived {
                continue;
            }
            let text = column_row_text(c);
            if tile.active == Some(ix) {
                active = Some(text.clone());
            }
            options.push(text);
            names.push(c.name.clone());
        }
        if names.is_empty() {
            return None;
        }
        let mut list = ChoiceList::new(options, choice::DEFAULT_CAP);
        list.place(active.as_deref());
        Some(Self {
            list,
            target: Target::Column {
                domain,
                view: tile.view.clone(),
                names,
            },
        })
    }
```

Free function beside `grouping_rows`:

```rust
/// A column row: the painted label, then the column name when they differ,
/// so typing either filters to it and two equal labels stay distinct.
fn column_row_text(c: &TileColumn) -> String {
    if c.label == c.name {
        c.name.clone()
    } else {
        format!("{} · {}", c.label, c.name)
    }
}
```

`pick_at` arm:

```rust
            Target::Column { domain, view, names } => Pick::Column {
                domain: *domain,
                view: view.clone(),
                column: names[declared].clone(),
            },
```

`title()` arm:

```rust
            Target::Column { domain, view, .. } => match domain {
                Domain::Schema => format!("Edit column in schema \u{b7} {view}").into(),
                _ => format!("Edit column in view \u{b7} {view}").into(),
            },
```

`highlighted_slot`: add `| Pick::Column { .. }` to the `None` arm. Fix every other non-exhaustive `match` on `Target`/`Pick` the compiler reports (`cargo check -p geode-shell --all-targets`).

- [ ] **Step 4: Run the pure tests**

Run: `cargo test -p geode-shell choicedialog::tests`
Expected: PASS.

- [ ] **Step 5: Chrome, opener, commit**

Hints constant beside `TILE_HINTS`:

```rust
const COLUMN_HINTS: &[Hint] = &[
    Hint::Text("type to filter ·"),
    Hint::Key("up"),
    Hint::Key("down"),
    Hint::Text("move ·"),
    Hint::Key("enter"),
    Hint::Text("edit ·"),
    Hint::Key("escape"),
    Hint::Text("close"),
];
```

`chrome` arm (fallback title only; `title()` supplies the real one):

```rust
        Target::Column { .. } => ("Edit column", "column", "column-hints", COLUMN_HINTS),
```

Notices and opener (after `open_tile_kinds_with`):

```rust
/// Status notice: the focused tile presents no configured view's columns.
pub(crate) const NO_TILE_COLUMNS: &str = "this tile has no dataset columns";
/// Status notice: every column of the tile's view is derived, so Schema has
/// nothing to open.
pub(crate) const NO_SCHEMA_COLUMNS: &str = "no schema columns in this tile's view";

/// Open the column list for `domain` (Views or Schema) over the focused
/// tile's columns — `config::view_column` / `config::schema_column`. The
/// target dialog's stack refusal runs first, so a list is never offered for
/// a dialog that could not open on its pick.
pub fn open_columns(
    view: &mut ShellView,
    domain: Domain,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    let tile = view
        .services
        .workspaces
        .active()
        .focused_tile()
        .and_then(|t| view.occupants.get(&t))
        .and_then(|o| o.content.tile_columns(cx));
    let Some(tile) = tile else {
        view.notice = Some(NO_TILE_COLUMNS);
        cx.notify();
        return;
    };
    if !dialog::can_open_object(view, domain) {
        cx.notify();
        return;
    }
    let Some(state) = ChoiceDialogState::columns(domain, &tile) else {
        view.notice = Some(NO_SCHEMA_COLUMNS);
        cx.notify();
        return;
    };
    open(view, state, window, cx);
}
```

`commit` arm:

```rust
        Pick::Column {
            domain,
            view,
            column,
        } => {
            // Close first so the object dialog pushes onto the stack the list
            // was opened over, not onto the list.
            shell.close_modal(window, cx);
            objectdialog::render::open_column(shell, domain, &view, &column, window, cx);
        }
```

Remove any `#[allow(dead_code)]` added in Task 3.

- [ ] **Step 6: Register and dispatch**

`defaults.rs`, after the `config::schema` line:

```rust
    // Open Views or Schema on one column of the focused tile's view: a list of
    // its columns, the cursor's highlighted. Palette-only.
    action(
        reg,
        "config::view_column",
        "Edit column in view…",
        "Configuration",
    );
    action(
        reg,
        "config::schema_column",
        "Edit column in schema…",
        "Configuration",
    );
```

`input.rs`, after the `config::expressions` arm:

```rust
        } else if action.0 == "config::view_column" {
            // Pull the focused tile's columns now; the list keeps this copy.
            choicedialog::open_columns(self, objectdialog::Domain::Views, window, cx);
        } else if action.0 == "config::schema_column" {
            choicedialog::open_columns(self, objectdialog::Domain::Schema, window, cx);
```

`dialog.rs` `opens_dialog`: add `| "config::view_column" | "config::schema_column"` after `"config::expressions"`.

`tests/dialog_stack.rs`:

```rust
    const REFUSES_IN_FIXTURE: &[&str] = &[
        // No focused tile answers `tile_columns` here: a status notice, no list.
        "config::view_column",
        "config::schema_column",
    ];
```

- [ ] **Step 7: Write the GPUI tests** — `crates/geode-shell/src/shell/tests/edit_column.rs`:

```rust
//! `config::view_column` / `config::schema_column` through the palette: the
//! focused tile's columns in a list, the cursor's preselected, a pick landing
//! on that column's Column stage.

use super::objectdialog::{desk_view_services, dialog_state};
use super::*;
use crate::defaults::AddPlacement;
use crate::module::recording::RecordingFactory;
use crate::shell::choicedialog::{NO_TILE_COLUMNS, Target};
use crate::shell::dialog::DialogKind;
use crate::shell::objectdialog::{Domain, Stage};
use geode_core::tile_columns::{TileColumn, TileColumns};

/// Open the palette, type `title`, press Enter: the trader's route.
fn open_palette_action(cx: &mut gpui::VisualTestContext, title: &str) {
    cx.simulate_keystrokes("ctrl-k");
    cx.simulate_input(title);
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
}

/// The desk view fixture (`tree` over `risk_snapshot`: `book`, `npv` labelled
/// `NPV`) with a focused "rec" tile answering `columns`.
fn shell_with_tile(
    cx: &mut gpui::TestAppContext,
    columns: Option<TileColumns>,
) -> (Entity<ShellView>, gpui::VisualTestContext) {
    let rec = RecordingFactory::new("rec");
    *rec.tile_columns.borrow_mut() = columns;
    let mut services = services_with_recorders(vec![rec]);
    let desk = desk_view_services(&[]);
    (services.config, services.builtin) = (desk.config, desk.builtin);
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    vcx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.add_tile("rec", AddPlacement::Split(None), None, window, cx);
        })
    });
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    vcx.run_until_parked();
    (shell, vcx)
}

fn tree(active: Option<usize>) -> TileColumns {
    TileColumns {
        view: "tree".into(),
        columns: vec![
            TileColumn { name: "book".into(), label: "book".into(), derived: false },
            TileColumn { name: "npv".into(), label: "NPV".into(), derived: false },
        ],
        active,
    }
}

fn stage(shell: &Entity<ShellView>, cx: &gpui::VisualTestContext) -> (Domain, Stage) {
    dialog_state(shell, cx, |s| (s.domain, s.stage.clone()))
}

#[gpui::test]
fn enter_opens_views_on_the_cursor_column(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = shell_with_tile(cx, Some(tree(Some(1))));
    open_palette_action(&mut cx, "Edit column in view");
    assert!(
        shell.read_with(&cx, |s, _| matches!(
            s.choice_dialog.as_ref().map(|d| &d.target),
            Some(Target::Column { domain: Domain::Views, .. })
        )),
        "the column list is open"
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        stage(&shell, &cx),
        (
            Domain::Views,
            Stage::Column { object: "tree".into(), column: "npv".into() }
        )
    );
    assert!(shell.read_with(&cx, |s, _| s.choice_dialog.is_none()), "the list closed");
}

#[gpui::test]
fn typing_picks_a_different_column(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = shell_with_tile(cx, Some(tree(Some(1))));
    open_palette_action(&mut cx, "Edit column in view");
    cx.simulate_input("book");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        stage(&shell, &cx),
        (
            Domain::Views,
            Stage::Column { object: "tree".into(), column: "book".into() }
        )
    );
}

#[gpui::test]
fn schema_opens_the_owning_dataset_on_the_column(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = shell_with_tile(cx, Some(tree(Some(1))));
    open_palette_action(&mut cx, "Edit column in schema");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        stage(&shell, &cx),
        (
            Domain::Schema,
            Stage::Column { object: "risk_snapshot".into(), column: "npv".into() }
        )
    );
}

#[gpui::test]
fn escape_steps_back_to_the_view_then_browse(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = shell_with_tile(cx, Some(tree(Some(1))));
    open_palette_action(&mut cx, "Edit column in view");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(
        stage(&shell, &cx),
        (Domain::Views, Stage::Edit { object: "tree".into() })
    );
}

#[gpui::test]
fn a_tile_without_columns_gets_a_notice_and_no_list(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = shell_with_tile(cx, None);
    open_palette_action(&mut cx, "Edit column in view");
    shell.read_with(&cx, |s, _| {
        assert!(s.choice_dialog.is_none());
        assert!(s.object_dialog.is_none());
        assert_eq!(s.notice, Some(NO_TILE_COLUMNS));
    });
}

#[gpui::test]
fn a_covered_views_dialog_refuses_before_the_list_opens(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = shell_with_tile(cx, Some(tree(Some(1))));
    open_palette_action(&mut cx, "Edit views");
    open_palette_action(&mut cx, "Edit colors");
    open_palette_action(&mut cx, "Edit column in view");
    shell.read_with(&cx, |s, _| {
        assert!(s.choice_dialog.is_none(), "no list for a dialog that cannot open");
        assert_eq!(s.notice, Some(Domain::Views.already_open_notice()));
        assert_eq!(s.top_kind(), Some(DialogKind::Object));
    });
}

#[gpui::test]
fn an_undefined_view_lands_in_browse_with_a_notice(cx: &mut gpui::TestAppContext) {
    let mut gone = tree(Some(1));
    gone.view = "gone".into();
    let (shell, mut cx) = shell_with_tile(cx, Some(gone));
    open_palette_action(&mut cx, "Edit column in view");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    dialog_state(&shell, &cx, |s| {
        assert_eq!(s.domain, Domain::Views);
        assert_eq!(s.stage, Stage::Browse);
        assert_eq!(s.notice.as_deref(), Some("view 'gone' is not defined"));
    });
}

#[gpui::test]
fn a_column_the_view_lacks_stops_at_its_edit_stage(cx: &mut gpui::TestAppContext) {
    let mut stale = tree(Some(0));
    stale.columns[0] = TileColumn {
        name: "vanished".into(),
        label: "vanished".into(),
        derived: false,
    };
    let (shell, mut cx) = shell_with_tile(cx, Some(stale));
    open_palette_action(&mut cx, "Edit column in view");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    dialog_state(&shell, &cx, |s| {
        assert_eq!(s.stage, Stage::Edit { object: "tree".into() });
        assert_eq!(s.notice.as_deref(), Some("'vanished' is not a column of 'tree'"));
    });
}

#[gpui::test]
fn enter_with_every_row_filtered_out_commits_nothing(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = shell_with_tile(cx, Some(tree(Some(1))));
    open_palette_action(&mut cx, "Edit column in view");
    cx.simulate_input("zzzz");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    shell.read_with(&cx, |s, _| {
        assert!(s.choice_dialog.is_some(), "the list stays open");
        assert!(s.object_dialog.is_none());
    });
}
```

Notes for the implementer:
- `Stage` must derive `PartialEq`/`Debug` and `ObjectDialogState.stage`/`notice` must be readable from `shell::tests` (they are crate-visible fields today; check `objectdialog/mod.rs:84` and `:3006`). If `Stage::Browse` carries fields, match with `matches!(s.stage, Stage::Browse { .. })` instead.
- If `ShellView::top_kind` is private to `shell`, tests under `shell::tests` can still call it (child module).
- If the palette ranks "Edit views…" above "Edit column in view…" for the typed query, type the full title including `…`.
- If `desk_view_services` is not `pub(super)`-reachable, it is (`tests/objectdialog.rs:340`); `dialog_state` is at `:55`.

- [ ] **Step 8: Run the GPUI tests**

Run: `cargo test -p geode-shell edit_column && cargo test -p geode-shell opens_dialog_matches_what_dispatch_pushes`
Expected: PASS. If a test fails, debug the production route (do not change assertions to match surprising behaviour without checking it against the spec).

- [ ] **Step 9: Full crate check**

Run: `cargo test -p geode-shell && cargo clippy -p geode-shell --all-targets -- -D warnings && cargo check -p geode-shell --features test-support --all-targets`
Expected: PASS.

- [ ] **Step 10: Commit**

```bash
git add crates/geode-shell
git commit -m "feat(shell): edit a tile's column in Views or Schema from the palette

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: Mutation entries, documentation, TODO

**Files:**
- Modify: `scripts/mutation-check.sh` (append near the object-stack entries, ~2937–2950)
- Modify: `docs/current/configuration-dialogs.md`, `docs/current/input-and-dialogs.md`, `docs/current/features.md` (Blotter section ~121), `crates/geode-blotter/README.md`, `crates/geode-shell/README.md`
- Modify: `TODO.md` (untracked — edit only, do not `git add` it)

- [ ] **Step 1: Add mutation entries** (format: name, file, exact original line, replacement, crate, test filter). Copy each anchor line byte-exact from the source after Tasks 2–4 (indentation included); `--anchors-only` rejects a non-unique or missing anchor.

```sh
run_mutation "edit column: the cursor column is preselected" \
  crates/geode-shell/src/shell/choicedialog.rs \
  '        list.place(active.as_deref());' \
  '        list.place(None);' \
  geode-shell \
  the_cursor_column_is_preselected

run_mutation "edit column: schema lists derived columns" \
  crates/geode-shell/src/shell/choicedialog.rs \
  '            if domain == Domain::Schema && c.derived {' \
  '            if false {' \
  geode-shell \
  schema_omits_derived_columns

run_mutation "edit column: schema opens the view's dataset for a joined column" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '        Domain::Schema => DatasetPresentationSpec::owner_of(spec, column, schema)' \
  '        Domain::Schema => Some(spec.dataset.as_str())' \
  geode-shell \
  schema_resolves_a_joined_column_to_its_owning_dataset

run_mutation "edit column: the route stops at the edit stage" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '            if !enter_column_stage(shell, column, cx) {' \
  '            if true {' \
  geode-shell \
  enter_opens_views_on_the_cursor_column

run_mutation "edit column: the blotter reports the tree column as active" \
  crates/geode-blotter/src/core/plan.rs \
  '        let active = (cursor_col > 0 && cursor_col < self.columns.len()).then(|| cursor_col - 1);' \
  '        let active = (cursor_col < self.columns.len()).then(|| cursor_col.saturating_sub(1));' \
  geode-blotter \
  the_tree_column_is_no_active_column
```

(The joined-column mutation leaves `.map(str::to_string).ok_or_else(..)` applied to an `Option<&str>`, so it compiles; if `rustfmt` placed the `owner_of` call on a different line, anchor on the line as formatted.)

- [ ] **Step 2: Run the entries**

Run: `zsh scripts/mutation-check.sh --anchors-only && zsh scripts/mutation-check.sh "edit column"`
Expected: anchors valid; all five entries report the mutation CAUGHT. Commit (or stash-safe) Tasks 1–4 before this step — the harness edits tracked files in place.

- [ ] **Step 3: Documentation**

- `docs/current/configuration-dialogs.md`, after the Stages table paragraph on Column/Values stages ("Column and Values stages stash the parent fields…"), add:

  > A Column stage can also be entered directly from a tile. `config::view_column` and `config::schema_column` open a list of the focused tile's presented columns (Schema omits derived columns, which no dataset declares), the cursor's column highlighted. A pick opens Views on the tile's view, or Schema on the dataset that owns the column — the primary dataset, then joins in declaration order — and enters that column's stage, resolved against configuration with pending edits folded in. An undefined view or an undeclared column stays in Browse with a footer notice; a column the object no longer has stops on the object's Edit stage with a notice. Escape then walks the usual ladder: Edit, Browse, close.

- `docs/current/input-and-dialogs.md`, under "Palette and which-key" (~177), add a short subsection "Edit column in view / schema": palette-only actions, no default binding; the list is a choice dialog (type to filter, Enter or click commits, Escape closes); status notices "this tile has no dataset columns" and "no schema columns in this tile's view"; the target dialog's stack refusal runs before the list opens.
- `docs/current/features.md` Blotter section: one bullet — the blotter answers `TileContent::tile_columns` with its planned non-tree columns (hidden columns and grouped dimensions are not offered) and the cursor's column.
- `crates/geode-blotter/README.md`: add `tile_columns` next to wherever `launch_context` is listed.
- `crates/geode-shell/README.md`: list `TileContent::tile_columns` beside `launch_context`, and `open_column` beside `open_object` if the README maps `objectdialog/render.rs`.
- `TODO.md`: delete the line `* Blotter shortcut to edit column in either view or schema`.

- [ ] **Step 4: Full workspace verification**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add scripts/mutation-check.sh docs crates/geode-blotter/README.md crates/geode-shell/README.md
git commit -m "docs, test: edit column from a tile — guides, READMEs, mutation entries

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

## Display checks (for Matthew, after merge)

- Palette: both titles appear under Configuration.
- The list: title `Edit column in view · tree`, rows `NPV · npv` style, the cursor's column highlighted on open, footer hints.
- A blotter with the cursor on the tree column: list opens on its first row.
