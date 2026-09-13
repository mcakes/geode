# Dataset-level column presentation — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A column's presentation (label, width, scale, precision, thousands, negative, colour) can be set once per dataset column, in a second personal overlay that every view of that dataset paints by, with the existing per-view overlay as the override on top; edited from the Schema dialog through the same column stage the Views dialog has.

**Architecture:** `dataset_presentation.toml` is read by `DatasetPresentationSpec` and merged into every `ViewSpec.presentation` map in `load_views` between the desk keys and the view overlay, so the blotter's plan builder and the reload path need no change. The object dialog gains `Destination::DatasetPresentation`, a stage-aware `Domain::writable(stage)`, a `ColumnContext` on the draft that lets one `Stage::Column` serve two doors (Views' member list, Schema's column rows), and a three-valued `Provenance` chip per field. The Views stage's baseline becomes desk + dataset.

**Tech Stack:** Rust, gpui + gpui-component (pinned rev `0e2fb7a`), `toml`/`toml_edit`, the mutation harness `scripts/mutation-check.sh`.

**Spec:** `docs/superpowers/specs/2026-09-13-geode-dataset-column-presentation-design.md` (§1–§8). Read it first; the plan argues from it. Two plan-level rulings amend it, recorded in Task 6: the dataset stage's per-field chip reads `dataset` rather than the spec's `user` (§4.3), so both doors share one `Provenance` vocabulary; and a member-row CLICK in the Views edit stage now opens the column stage like `enter` (closing the 2c ledger's deferred minor), since the Schema door's click parity uses the same handler.

## Global Constraints

- Work in a git worktree on a branch off `main` (currently `f416013`). Run every command from the worktree root as a plain single command — the worktree guard refuses compound shell containing `git` or `zsh`. NEVER background a cargo command; run it in the foreground and wait.
- Five CI checks must be green before every commit that touches code: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo bench --workspace --no-run`, `cargo check -p geode-shell --features test-support --all-targets`.
- **Harness rules:** a `run_mutation` entry for every load-bearing line you add or move, appended immediately after the last existing entry (`objectdialog: a keystroke in a plain field keeps the cursor on its row`, before the `if [[ -n "$changed_ref" ]]` block), naming its covering test as the 6th argument. Every anchor must occur exactly once in its file (`grep -c -F '<anchor>' <file>` prints `1`). `zsh scripts/mutation-check.sh --anchors-only` must exit 0 before every commit — re-anchor any pre-existing entry your edit moves (Task 3 and Task 5 name the ones they move). **Commit before running any mutation entry** (`git checkout` restores the file). Run each new entry by name and confirm `caught`; the full `--changed=main` run is the controller's, detached, after Task 6.
- Interaction-model rule (CLAUDE.md): pure state is the truth; a transition site mutates `mode`/`query` only and never calls `focus` or `set_value` — `dialog::sync_dialog_text` is the only writer. A new mouse handler that ends a transition must end in `sync_dialog_text`.
- The `apply::config_with_pending` rule: a stage that reads a doc the dialog itself writes reads it through `apply::config_with_pending(shell)` (falling back to `services.config`), never `services.config` alone (2c final review M-6).
- `hidden` and `order` are view-only; the dataset overlay refuses both with a warning naming `view_presentation.toml`.
- Commit messages end with:
  ```
  Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_01Acu5TGjiKbL1bZ2Mo9W7yY
  ```
- Do not touch `TODO.md` or `docs/modules.md`.

## File structure

| File | Responsibility |
|---|---|
| `crates/geode-core/src/view.rs` | `DatasetPresentationSpec { from_doc, apply }` beside `ViewPresentationSpec`; `ColumnPresentation` unchanged |
| `crates/geode-core/src/config/load.rs` | `load_views`: parse the schema, apply the dataset overlay between desk keys and the view overlay, cross-check its colours |
| `crates/geode-core/src/config/merge.rs` | `atomic_depth("dataset_presentation") = Some(1)` |
| `crates/geode-shell/src/shell/hot_reload.rs` | `views_changed` gains `changed("dataset_presentation")` |
| `crates/geode-shell/src/shell/objectdialog/mod.rs` | `Destination::DatasetPresentation`, `Domain::writable(stage)`, `Provenance`, `ColumnDoor`, `ColumnLayers`, `ColumnContext`, `Draft.column_ctx`, `Draft::enter_column` membership via a `columns.<col>` field, `fold_column` over either door, `Fold`/`FellTo` |
| `crates/geode-shell/src/shell/objectdialog/dataset_columns.rs` (new) | the Schema door's pure half: `DOC`, `overlay_object`, `item_for`, `table`, `row_summary`, `provenance_of` |
| `crates/geode-shell/src/shell/objectdialog/views.rs` | `baseline_below`, `column_fields(item, colours, dest)`, `fold_into` returning the cleared key, helper visibility widened to `pub(super)` |
| `crates/geode-shell/src/shell/objectdialog/schema.rs` | `fields` appends the dataset-level summary to a column row; `to_table` dispatches `DatasetPresentation` to `dataset_columns::table` |
| `crates/geode-shell/src/shell/objectdialog/apply.rs` | `object_value`: `Remove` for `DatasetPresentation` |
| `crates/geode-shell/src/shell/objectdialog/render.rs` | `enter_column_stage` with a Schema arm, `commit_selected_row` opening from a Schema column row, `on_edit_row_clicked` opening a column stage, `leave_column_stage` refreshing the Schema row, the fold notice naming its layer, the provenance chip, `writable(&state.stage)` at every site |
| `crates/geode-shell/src/shell/tests/objectdialog.rs`, `tests/reload.rs`, `crates/geode-app/src/bridge.rs` tests | window, reload and bridge tests |
| `scripts/mutation-check.sh`, `CLAUDE.md`, the spec | harness entries, the paragraph, `## 9. As built` |

---

### Task 1: Core — `DatasetPresentationSpec`, the merge point, the cross-checks

**Files:**
- Modify: `crates/geode-core/src/view.rs` (after `ViewPresentationSpec`'s `impl`, ~line 1010)
- Modify: `crates/geode-core/src/config/load.rs:104-159` (`load_views`)
- Modify: `crates/geode-core/src/config/merge.rs:22` (`atomic_depth`)
- Test: both files' `mod tests`
- Modify: `scripts/mutation-check.sh` (append)

**Interfaces:**
- Consumes: `ColumnPresentation::{parse_format_keys, parse_column_keys, merge_over}`, `ViewSpec { dataset, joins: Vec<JoinSpec { dataset, .. }>, columns, presentation }`, `SchemaSpec::{from_doc, dataset(name) -> Option<&DatasetSpec>}`, `DatasetSpec::column(name)`.
- Produces: `pub struct DatasetPresentationSpec { pub datasets: BTreeMap<String, BTreeMap<String, ColumnPresentation>> }`, `DatasetPresentationSpec::from_doc(doc: &MergedDoc) -> (Self, Vec<Diagnostic>)`, `DatasetPresentationSpec::apply(&self, views: &mut [ViewSpec], schema: &SchemaSpec) -> Vec<Diagnostic>`, `DatasetPresentationSpec::owner_of(view: &ViewSpec, column: &str, schema: &SchemaSpec) -> Option<&str>` (the dataset that declares the column, own first then joins), `pub const DATASET_PRESENTATION_DOC: &str = "dataset_presentation"`.

- [ ] **Step 1: Write the failing reader tests** in `crates/geode-core/src/view.rs`'s `mod tests` (append before the module's closing brace):

```rust
    fn dataset_doc(text: &str) -> crate::config::MergedDoc {
        merge_docs(
            "dataset_presentation",
            &[LayerDoc::builtin("dataset_presentation", text).unwrap()],
        )
    }

    #[test]
    fn dataset_presentation_reads_one_table_per_column_with_paths() {
        let (spec, diags) = DatasetPresentationSpec::from_doc(&dataset_doc(
            "[risk.columns.delta01]\nlabel = \"Δ\"\nwidth = 90\nscale = \"k\"\nprecision = 0\n\
             thousands = true\nnegative = \"parens\"\ncolour = \"delta\"\n\
             [risk.columns.npv]\nwidth = \"wide\"\n",
        ));
        let delta = &spec.datasets["risk"]["delta01"];
        assert_eq!(delta.label.as_deref(), Some("Δ"));
        assert_eq!(delta.width, Some(90.0));
        assert_eq!(delta.scale, Some(Scale::Thousands));
        assert_eq!(delta.precision, Some(0));
        assert_eq!(delta.thousands, Some(true));
        assert_eq!(delta.negative, Some(Negative::Parens));
        assert_eq!(delta.colour, Some(Colour::Named("delta".into())));
        assert_eq!(delta.hidden, None, "the dataset overlay never carries hidden");
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(
            diags[0].path.as_deref(),
            Some("dataset_presentation.risk.columns.npv.width")
        );
        assert!(spec.datasets["risk"].contains_key("npv"), "a bad key skips the key, not the column");
    }

    #[test]
    fn dataset_presentation_refuses_hidden_and_order_naming_the_view_overlay() {
        let (spec, diags) = DatasetPresentationSpec::from_doc(&dataset_doc(
            "[risk]\norder = [\"npv\"]\n[risk.columns.delta01]\nhidden = true\nscale = \"k\"\n",
        ));
        assert_eq!(spec.datasets["risk"]["delta01"].scale, Some(Scale::Thousands));
        assert_eq!(spec.datasets["risk"]["delta01"].hidden, None);
        let messages: Vec<&str> = diags.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(diags.len(), 2, "{messages:?}");
        assert!(messages.iter().all(|m| m.contains("view_presentation.toml")), "{messages:?}");
        assert_eq!(diags[0].path.as_deref(), Some("dataset_presentation.risk.order"));
        assert_eq!(
            diags[1].path.as_deref(),
            Some("dataset_presentation.risk.columns.delta01.hidden")
        );
    }

    #[test]
    fn dataset_presentation_skips_a_non_table_dataset_and_column() {
        let (spec, diags) = DatasetPresentationSpec::from_doc(&dataset_doc(
            "config_version = 1\nrisk = 3\n[vol.columns]\nstrike = \"no\"\n[vol.columns.spot]\nwidth = 80\n",
        ));
        assert!(!spec.datasets.contains_key("risk"));
        assert_eq!(spec.datasets["vol"]["spot"].width, Some(80.0));
        assert!(!spec.datasets["vol"].contains_key("strike"));
        let paths: Vec<&str> = diags.iter().filter_map(|d| d.path.as_deref()).collect();
        assert_eq!(
            paths,
            vec!["dataset_presentation.risk", "dataset_presentation.vol.columns.strike"]
        );
    }
```

Add `Scale` and `Negative` to the test module's imports if `use super::*;` does not already cover them (it does — they are defined in this file).

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-core dataset_presentation_ 2>&1 | grep -E "^error|test result" | head -3`
Expected: `error[E0433]: ... DatasetPresentationSpec` (not defined).

- [ ] **Step 3: Implement the reader** in `crates/geode-core/src/view.rs`, directly after `impl ViewPresentationSpec { ... }` ends (before `#[cfg(test)]`):

```rust
/// `dataset_presentation.toml`, user layer — one table per DATASET name,
/// atomic at depth one like `view_presentation` (`config::merge::
/// atomic_depth`), holding one `[<dataset>.columns.<col>]` table per
/// personalised column: the seven presentation keys and nothing else.
/// `hidden` and `order` belong to a view and are refused here with a
/// warning naming `view_presentation.toml` (dataset-presentation spec
/// §2.1). Merged into every view of that dataset BETWEEN the view's own
/// `[[columns]]` keys and the trader's view overlay (§3.1), so the
/// resolved order per key is kind default → desk view column →
/// dataset-level → view-level.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DatasetPresentationSpec {
    /// dataset → column → presentation
    pub datasets: BTreeMap<String, BTreeMap<String, ColumnPresentation>>,
}

pub const DATASET_PRESENTATION_DOC: &str = "dataset_presentation";

impl DatasetPresentationSpec {
    pub fn from_doc(doc: &MergedDoc) -> (DatasetPresentationSpec, Vec<Diagnostic>) {
        let mut spec = DatasetPresentationSpec::default();
        let mut diags = Vec::new();
        for (dataset, value) in &doc.value {
            if dataset == "config_version" {
                continue;
            }
            let at = |suffix: &str| {
                if suffix.is_empty() {
                    format!("{DATASET_PRESENTATION_DOC}.{dataset}")
                } else {
                    format!("{DATASET_PRESENTATION_DOC}.{dataset}.{suffix}")
                }
            };
            let bad = |suffix: &str, m: String| Diagnostic {
                severity: Severity::Warning,
                layer: None,
                file: None,
                message: format!("dataset presentation '{dataset}': {m}"),
                path: Some(at(suffix)),
            };
            let Some(table) = value.as_table() else {
                diags.push(bad("", "not a table".into()));
                continue;
            };
            let mut columns = BTreeMap::new();
            for (key, v) in table {
                match key.as_str() {
                    "columns" => {
                        let Some(cols) = v.as_table() else {
                            diags.push(bad(
                                "columns",
                                "'columns' must be a table of column tables".into(),
                            ));
                            continue;
                        };
                        for (col, cv) in cols {
                            let Some(ct) = cv.as_table() else {
                                diags.push(bad(
                                    &format!("columns.{col}"),
                                    format!("column '{col}': not a table"),
                                ));
                                continue;
                            };
                            let mut cp = ColumnPresentation::default();
                            let col_diags = RefCell::new(Vec::new());
                            let warn = |k: &str, m: String| {
                                col_diags.borrow_mut().push(bad(
                                    &format!("columns.{col}.{k}"),
                                    format!("column '{col}': {m}"),
                                ));
                            };
                            cp.parse_format_keys(ct, &warn);
                            // `read_hidden: false`: the dataset overlay
                            // never carries membership.
                            cp.parse_column_keys(ct, false, &warn);
                            if ct.contains_key("hidden") {
                                warn(
                                    "hidden",
                                    "'hidden' belongs to a view — set it in view_presentation.toml"
                                        .into(),
                                );
                            }
                            diags.extend(col_diags.into_inner());
                            columns.insert(col.clone(), cp);
                        }
                    }
                    "order" => diags.push(bad(
                        "order",
                        "'order' belongs to a view — set it in view_presentation.toml".into(),
                    )),
                    other => diags.push(bad(other, format!("unknown key '{other}' — ignored"))),
                }
            }
            spec.datasets.insert(dataset.clone(), columns);
        }
        (spec, diags)
    }

    /// The dataset whose schema declares `column` for this view: the
    /// view's own dataset first, then each join's dataset in file order
    /// — the order the compiler resolves names in (§3.2). `None` for a
    /// column no dataset of the view declares (a derived column).
    pub fn owner_of<'a>(view: &'a ViewSpec, column: &str, schema: &SchemaSpec) -> Option<&'a str> {
        std::iter::once(view.dataset.as_str())
            .chain(view.joins.iter().map(|j| j.dataset.as_str()))
            .find(|ds| schema.dataset(ds).is_some_and(|d| d.column(column).is_some()))
    }

    /// Merge each dataset's column tables over the matching column of
    /// every view that carries it (§3.1). Called by `load_views` AFTER
    /// `ViewSpec::from_doc` (the desk's own keys are already in
    /// `presentation`) and BEFORE `ViewPresentationSpec::apply` (the
    /// view overlay must win). A dataset no schema declares, or a column
    /// its dataset lacks, warns with its path and is skipped (§2.3).
    pub fn apply(&self, views: &mut [ViewSpec], schema: &SchemaSpec) -> Vec<Diagnostic> {
        let mut diags = Vec::new();
        let warn = |path: String, m: String| Diagnostic {
            severity: Severity::Warning,
            layer: None,
            file: None,
            message: m,
            path: Some(path),
        };
        for (dataset, columns) in &self.datasets {
            let Some(spec) = schema.dataset(dataset) else {
                diags.push(warn(
                    format!("{DATASET_PRESENTATION_DOC}.{dataset}"),
                    format!("dataset presentation '{dataset}': names dataset '{dataset}', which no schema declares — ignored"),
                ));
                continue;
            };
            for (col, cp) in columns {
                if spec.column(col).is_none() {
                    diags.push(warn(
                        format!("{DATASET_PRESENTATION_DOC}.{dataset}.columns.{col}"),
                        format!("dataset presentation '{dataset}': names column '{col}', which dataset '{dataset}' does not have — ignored"),
                    ));
                    continue;
                }
                for view in views.iter_mut() {
                    let owned_here = view
                        .columns
                        .iter()
                        .any(|c| c.name() == col)
                        && Self::owner_of(view, col, schema) == Some(dataset.as_str());
                    if owned_here {
                        view.presentation
                            .entry(col.clone())
                            .or_default()
                            .merge_over(cp);
                    }
                }
            }
        }
        diags
    }
}
```

`owner_of` borrows the dataset name out of `view`, so `apply` compares it with `dataset.as_str()`; the `views.iter_mut()` borrow and the `owner_of(view, ..)` shared borrow do not overlap because `owned_here` is computed before the `entry` call — if the borrow checker objects, compute `let owner = Self::owner_of(view, col, schema).map(str::to_string);` first.

- [ ] **Step 4: Run the reader tests**

Run: `cargo test -p geode-core dataset_presentation_ 2>&1 | grep -E "^error|test result" | head -3`
Expected: `test result: ok. 3 passed`.

- [ ] **Step 5: Write the failing merge-order and ownership tests** (same test module):

```rust
    fn schema_with(text: &str) -> SchemaSpec {
        SchemaSpec::from_doc(&merge_docs(
            "datasets",
            &[LayerDoc::builtin("datasets", text).unwrap()],
        ))
        .0
    }

    const RISK_SCHEMA: &str = "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
        [risk.columns.delta01]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n\
        [risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n";

    #[test]
    fn dataset_level_beats_the_desk_view_and_loses_to_the_view_level() {
        let schema = schema_with(RISK_SCHEMA);
        let (mut views, _) = ViewSpec::from_doc(&doc(
            "[tree]\ndataset = \"risk\"\ngrouping = [\"book\"]\n\
             [[tree.columns]]\nname = \"book\"\nkind = \"dimension\"\n\
             [[tree.columns]]\nname = \"delta01\"\nkind = \"measure\"\nlabel = \"desk\"\nwidth = 50\n\
             format = { scale = \"m\", precision = 2 }\n",
        ));
        let (dataset, d) = DatasetPresentationSpec::from_doc(&dataset_doc(
            "[risk.columns.delta01]\nlabel = \"dataset\"\nscale = \"k\"\ncolour = \"delta\"\n",
        ));
        assert!(d.is_empty(), "{d:?}");
        assert!(dataset.apply(&mut views, &schema).is_empty());
        let p = views[0].presentation_of("delta01");
        assert_eq!(p.label.as_deref(), Some("dataset"), "dataset beats the desk view");
        assert_eq!(p.scale, Some(Scale::Thousands));
        assert_eq!(p.colour, Some(Colour::Named("delta".into())));
        assert_eq!(p.width, Some(50.0), "an unset dataset key keeps the desk's");
        assert_eq!(p.precision, Some(2));

        let view_doc = merge_docs(
            "view_presentation",
            &[LayerDoc::builtin(
                "view_presentation",
                "[tree.columns.delta01]\nlabel = \"view\"\nscale = \"units\"\n",
            )
            .unwrap()],
        );
        let (view_overlay, _) = ViewPresentationSpec::from_doc(&view_doc);
        assert!(view_overlay.apply(&mut views).is_empty());
        let p = views[0].presentation_of("delta01");
        assert_eq!(p.label.as_deref(), Some("view"), "the view level beats the dataset level");
        assert_eq!(p.scale, Some(Scale::None));
        assert_eq!(p.colour, Some(Colour::Named("delta".into())), "an unset view key keeps the dataset's");
    }

    #[test]
    fn a_joined_column_takes_its_own_datasets_entry_and_the_view_dataset_wins_a_tie() {
        let schema = schema_with(
            "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
             [risk.columns.instrument_ref]\ntype = \"utf8\"\nrole = \"key\"\n\
             [risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n\
             [ref.columns.instrument_ref]\ntype = \"utf8\"\nrole = \"key\"\n\
             [ref.columns.spot]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"instrument\"\n\
             [ref.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"instrument\"\n",
        );
        let (mut views, _) = ViewSpec::from_doc(&doc(
            "[j]\ndataset = \"risk\"\ngrouping = [\"book\"]\n\
             [[j.joins]]\ndataset = \"ref\"\non = [\"instrument_ref\"]\n\
             [[j.columns]]\nname = \"npv\"\nkind = \"measure\"\n\
             [[j.columns]]\nname = \"spot\"\nkind = \"measure\"\n\
             [[j.columns]]\nname = \"vega\"\nkind = \"derived\"\nsql = \"npv * 2\"\n",
        ));
        assert_eq!(DatasetPresentationSpec::owner_of(&views[0], "spot", &schema), Some("ref"));
        assert_eq!(DatasetPresentationSpec::owner_of(&views[0], "npv", &schema), Some("risk"));
        assert_eq!(DatasetPresentationSpec::owner_of(&views[0], "vega", &schema), None);
        let (dataset, _) = DatasetPresentationSpec::from_doc(&dataset_doc(
            "[ref.columns.spot]\nwidth = 70\n[ref.columns.npv]\nwidth = 99\n[risk.columns.npv]\nwidth = 42\n",
        ));
        assert!(dataset.apply(&mut views, &schema).is_empty());
        assert_eq!(views[0].presentation_of("spot").width, Some(70.0), "a joined column takes the join's entry");
        assert_eq!(views[0].presentation_of("npv").width, Some(42.0), "the view's own dataset wins a tie");
    }

    #[test]
    fn an_unknown_dataset_or_column_warns_with_its_path_and_is_skipped() {
        let schema = schema_with(RISK_SCHEMA);
        let (mut views, _) = ViewSpec::from_doc(&doc(
            "[tree]\ndataset = \"risk\"\ngrouping = [\"book\"]\n[[tree.columns]]\nname = \"delta01\"\nkind = \"measure\"\n",
        ));
        let (dataset, _) = DatasetPresentationSpec::from_doc(&dataset_doc(
            "[ghost.columns.x]\nwidth = 1\n[risk.columns.nope]\nwidth = 2\n[risk.columns.delta01]\nwidth = 3\n",
        ));
        let diags = dataset.apply(&mut views, &schema);
        let paths: Vec<&str> = diags.iter().filter_map(|d| d.path.as_deref()).collect();
        assert_eq!(
            paths,
            vec!["dataset_presentation.ghost", "dataset_presentation.risk.columns.nope"]
        );
        assert_eq!(views[0].presentation_of("delta01").width, Some(3.0));
    }
```

If the joined-view fixture's `kind = "derived"` spelling differs from what `ViewSpec::from_doc` accepts, copy the spelling from the existing `SAMPLE` fixture in this test module (search it for `sql =`).

- [ ] **Step 6: Run them; expect the three to fail on `apply`'s absence or on assertion**, then confirm Step 3's `apply` makes them pass:

Run: `cargo test -p geode-core -- dataset_level_beats a_joined_column an_unknown_dataset 2>&1 | grep -E "^error|test result|panicked" | head -5`
Expected: `test result: ok. 3 passed`.

- [ ] **Step 7: Merge it in `load_views`** (`crates/geode-core/src/config/load.rs`). Change the import line and the body:

```rust
use crate::schema::SchemaSpec;
use crate::view::{Colour, DatasetPresentationSpec, ViewPresentationSpec, ViewSpec};
```

Immediately after `let (mut views, mut diags) = ViewSpec::from_doc(views_doc);` insert:

```rust
    // The dataset-level overlay merges BETWEEN the desk's own keys
    // (already in `presentation` from `from_doc`) and the view overlay
    // below, so the resolved order per key is kind default → desk view
    // column → dataset-level → view-level (dataset-presentation spec §3.1).
    let schema = config
        .doc("datasets")
        .map(|d| SchemaSpec::from_doc(d).0)
        .unwrap_or_default();
    let dataset_overlay = config.doc(crate::view::DATASET_PRESENTATION_DOC).map(|doc| {
        let (spec, d) = DatasetPresentationSpec::from_doc(doc);
        diags.extend(d);
        spec
    });
    if let Some(overlay) = &dataset_overlay {
        diags.extend(overlay.apply(&mut views, &schema));
    }
```

Then, inside the existing `if let Some(doc) = config.doc("view_presentation")` block's colour cross-check, add a matching cross-check for the dataset overlay just before it (after the `colours` binding):

```rust
    if let Some(overlay) = &dataset_overlay {
        for (dataset, columns) in &overlay.datasets {
            for (col, cp) in columns {
                let Some(Colour::Named(name)) = &cp.colour else {
                    continue;
                };
                if colours.get(name).is_none() {
                    diags.push(Diagnostic {
                        severity: Severity::Warning,
                        layer: None,
                        file: None,
                        message: format!(
                            "dataset presentation '{dataset}': column '{col}' names colour '{name}', which colours.toml does not define — painted in foreground"
                        ),
                        path: Some(format!(
                            "dataset_presentation.{dataset}.columns.{col}.colour"
                        )),
                    });
                }
            }
        }
    }
```

Note the `colours` binding must be computed before this block (move the `let colours = ...` lines above the dataset overlay code if needed).

- [ ] **Step 8: Register the doc's atomic depth** in `crates/geode-core/src/config/merge.rs:22`: add `| "dataset_presentation"` to the `Some(1)` arm, with a comment `// dataset_presentation (dataset-presentation spec §2.1): one table per dataset name, like view_presentation`.

- [ ] **Step 9: Write the failing `load_views` test** in `crates/geode-core/src/config/load.rs`'s `mod tests` (append; the module already has `Config`/`ConfigSources` imports and a `write` helper):

```rust
    #[test]
    fn load_views_merges_the_dataset_overlay_under_the_view_overlay_and_reports_its_colours() {
        let config = Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin(
                    "datasets",
                    "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n[risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n[risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n",
                )
                .unwrap(),
                LayerDoc::builtin(
                    "views",
                    "[v]\ndataset = \"risk\"\ngrouping = [\"book\"]\n[[v.columns]]\nname = \"book\"\nkind = \"dimension\"\n[[v.columns]]\nname = \"npv\"\nkind = \"measure\"\nwidth = 50\n[w]\ndataset = \"risk\"\ngrouping = [\"book\"]\n[[w.columns]]\nname = \"npv\"\nkind = \"measure\"\n",
                )
                .unwrap(),
                LayerDoc::builtin(
                    "dataset_presentation",
                    "[risk.columns.npv]\nwidth = 140\ncolour = \"nope\"\n",
                )
                .unwrap(),
                LayerDoc::builtin("view_presentation", "[v.columns.npv]\nwidth = 200\n").unwrap(),
                LayerDoc::builtin("colours", "[delta]\nhue = 240\n").unwrap(),
            ],
            ..ConfigSources::default()
        });
        let (views, diags) = load_views(&config);
        let v = views.iter().find(|v| v.name == "v").unwrap();
        let w = views.iter().find(|v| v.name == "w").unwrap();
        assert_eq!(v.presentation_of("npv").width, Some(200.0), "view overlay wins in v");
        assert_eq!(w.presentation_of("npv").width, Some(140.0), "dataset level reaches w");
        assert_eq!(
            w.presentation_of("npv").colour,
            Some(Colour::Named("nope".into())),
            "an unknown colour still merges; it is warned about, not dropped"
        );
        let colour_warning = diags
            .iter()
            .find(|d| d.path.as_deref() == Some("dataset_presentation.risk.columns.npv.colour"))
            .expect("the dataset overlay's colour is cross-checked");
        assert!(colour_warning.message.contains("nope"), "{}", colour_warning.message);
    }
```

- [ ] **Step 10: Run the core suite**

Run: `cargo test -p geode-core 2>&1 | grep -E "^error|test result|FAILED|panicked" | head -5`
Expected: all `ok`, the new test included.

- [ ] **Step 11: Format, lint, commit**

Run: `cargo fmt` then `cargo fmt --check` then `cargo clippy --workspace --all-targets -- -D warnings 2>&1 | grep -E "^(warning: unused|error)" | head -3` then `zsh scripts/mutation-check.sh --anchors-only`.

```bash
git add crates/geode-core scripts
git commit -m "core(view): dataset_presentation.toml — a second overlay merged under the view's (dataset-presentation spec §2–§3)"
```

- [ ] **Step 12: Harness entries.** Append after the last entry:

```bash
# dataset-presentation spec §3.1: the dataset overlay must MERGE over the
# desk's keys — replacing the entry would drop every desk key the
# dataset table leaves unset (width, precision in the test).
run_mutation "view: the dataset overlay merges over the desk keys rather than replacing them" \
  crates/geode-core/src/view.rs \
  '                        view.presentation
                            .entry(col.clone())
                            .or_default()
                            .merge_over(cp);' \
  '                        view.presentation.insert(col.clone(), cp.clone());' \
  geode-core \
  dataset_level_beats_the_desk_view_and_loses_to_the_view_level

# §3.2: own dataset first, then the joins — a column both declare takes
# the view's own dataset's entry, never the join's.
run_mutation "view: a column's owner is the view's own dataset before any join" \
  crates/geode-core/src/view.rs \
  '        std::iter::once(view.dataset.as_str())
            .chain(view.joins.iter().map(|j| j.dataset.as_str()))' \
  '        view.joins.iter().map(|j| j.dataset.as_str())
            .chain(std::iter::once(view.dataset.as_str()))' \
  geode-core \
  a_joined_column_takes_its_own_datasets_entry_and_the_view_dataset_wins_a_tie

# §3.1: the dataset overlay is applied BEFORE the view overlay, so the
# view level wins; applied after, the dataset level would win in `v`.
run_mutation "load: the dataset overlay is applied before the view overlay" \
  crates/geode-core/src/config/load.rs \
  '    if let Some(overlay) = &dataset_overlay {
        diags.extend(overlay.apply(&mut views, &schema));
    }' \
  '    let _ = &dataset_overlay;' \
  geode-core \
  load_views_merges_the_dataset_overlay_under_the_view_overlay_and_reports_its_colours
```

Check each anchor: `grep -c -F '<first line of anchor>' <file>` prints `1` (for a multi-line anchor, count the first line). The `merge_over(cp)` anchor's first line `view.presentation` occurs elsewhere in `view.rs` — if `grep -c` reports more than one, extend the anchor with the preceding line `if owned_here {`. Then:

Run: `zsh scripts/mutation-check.sh --anchors-only` → `0 stale, 0 ambiguous`.
Commit the harness (`git commit -am "mutation: entries for the dataset overlay's merge, ownership and ordering"`), then run each by name: `zsh scripts/mutation-check.sh "dataset overlay merges"`, `"column's owner"`, `"applied before the view overlay"` → each prints `caught`. A `SURVIVED` means the mutation did not break the test — fix the test, not the entry.

---

### Task 2: Reload predicate and the bridge

**Files:**
- Modify: `crates/geode-shell/src/shell/hot_reload.rs:281-284`
- Test: `crates/geode-shell/src/shell/tests/reload.rs` (model on `a_colours_change_fires_config_reloaded`, line 97)
- Test: `crates/geode-app/src/bridge.rs` `mod tests` (model on `data_setup_hands_out_views_with_the_users_presentation_already_merged`, line 1431)
- Modify: `scripts/mutation-check.sh` (append)

**Interfaces:**
- Consumes: `changed(name)` closure in `apply_reload`; `data_setup(config, db_path)` returning `DataSetup { views, .. }`.
- Produces: nothing new — the reload predicate and a test.

- [ ] **Step 1: Write the failing reload test.** Open `crates/geode-shell/src/shell/tests/reload.rs`, read `a_colours_change_fires_config_reloaded` (lines 91–145) in full, and add a sibling directly after it that is identical except: the doc name is `"dataset_presentation"`, the text is `"[risk.columns.npv]\nwidth = 140\n"`, the function is named `a_dataset_presentation_change_fires_config_reloaded`, and the assertion message reads `"a dataset_presentation-only reload must fire ConfigReloaded"`.

- [ ] **Step 2: Run it to see it fail**

Run: `cargo test -p geode-shell a_dataset_presentation_change_fires 2>&1 | grep -E "test result|panicked" | head -3`
Expected: `1 failed` — no `ConfigReloaded` emitted.

- [ ] **Step 3: Extend the predicate** in `hot_reload.rs`:

```rust
            let views_changed = changed("views")
                || changed("view_presentation")
                || changed("dataset_presentation")
                || changed("dimensions")
                || changed("colours");
```

and add to the comment block above it: `// dataset_presentation rides here for the same reason view_presentation does (dataset-presentation spec §6): load_views merges it into every ViewSpec of that dataset.`

- [ ] **Step 4: Run the test; expect it to pass.** Then run `cargo test -p geode-shell reload 2>&1 | grep -E "test result|FAILED" | head -3` → all ok.

- [ ] **Step 5: Bridge test.** In `crates/geode-app/src/bridge.rs`'s `mod tests`, after `data_setup_hands_out_views_with_the_users_presentation_already_merged`, add:

```rust
    #[test]
    fn data_setup_hands_out_views_with_the_dataset_level_merged_under_the_view_level() {
        let config = Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin(
                    "datasets",
                    "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n[risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n[risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n",
                )
                .unwrap(),
                LayerDoc::builtin(
                    "views",
                    "[v]\ndataset = \"risk\"\ngrouping = [\"book\"]\n[[v.columns]]\nname = \"book\"\nkind = \"dimension\"\n[[v.columns]]\nname = \"npv\"\nkind = \"measure\"\n[w]\ndataset = \"risk\"\ngrouping = [\"book\"]\n[[w.columns]]\nname = \"npv\"\nkind = \"measure\"\n",
                )
                .unwrap(),
                LayerDoc::builtin("dataset_presentation", "[risk.columns.npv]\nlabel = \"NPV k\"\nscale = \"k\"\n").unwrap(),
                LayerDoc::builtin("view_presentation", "[v.columns.npv]\nlabel = \"NPV\"\n").unwrap(),
            ],
            ..ConfigSources::default()
        });
        let setup = data_setup(&config, "/tmp/x.duckdb".into()).unwrap();
        let v = setup.views.iter().find(|v| v.name == "v").unwrap();
        let w = setup.views.iter().find(|v| v.name == "w").unwrap();
        assert_eq!(v.presentation_of("npv").label.as_deref(), Some("NPV"));
        assert_eq!(w.presentation_of("npv").label.as_deref(), Some("NPV k"));
        assert_eq!(v.presentation_of("npv").scale, Some(geode_core::view::Scale::Thousands), "an unset view key keeps the dataset's");
    }
```

Run: `cargo test -p geode-app data_setup_hands_out_views_with_the_dataset 2>&1 | grep -E "test result|panicked" | head -2` → `ok`.

- [ ] **Step 6: Format, lint, commit, harness.**

```bash
git add crates/geode-shell/src/shell/hot_reload.rs crates/geode-shell/src/shell/tests/reload.rs crates/geode-app/src/bridge.rs
git commit -m "shell(reload): a dataset_presentation change fans out like a view overlay change (spec §6)"
```

Append the entry (the anchor line `|| changed("dataset_presentation")` occurs once):

```bash
# dataset-presentation spec §6: omitted from the predicate, a dataset-level
# edit sits on disk until the next restart — the same failure the
# view_presentation and colours lines above it fixed.
run_mutation "reload: a dataset_presentation change fires ConfigReloaded" \
  crates/geode-shell/src/shell/hot_reload.rs \
  '                || changed("dataset_presentation")' \
  '                || false' \
  geode-shell \
  a_dataset_presentation_change_fires_config_reloaded
```

`--anchors-only` clean; commit the harness; run `zsh scripts/mutation-check.sh "dataset_presentation change fires"` → `caught`.

---

### Task 3: Dialog scaffold — destination, stage-aware gate, provenance, the column context

**Files:**
- Modify: `crates/geode-shell/src/shell/objectdialog/mod.rs` (`Destination` ~746, `Destination::doc` ~755, `Domain::writable` ~332, `Domain::text_editable`/`parse_text` ~2458–2490, `Field` ~911, `Draft` ~1069/1151–1155, `enter_column` ~1404, `fold_column` ~1466, `leave_column` ~1519)
- Modify: `crates/geode-shell/src/shell/objectdialog/apply.rs:244-250` (`object_value`)
- Modify: `crates/geode-shell/src/shell/objectdialog/views.rs` (`column_fields` ~848, `fold_into` ~1012, helper visibility)
- Modify: `crates/geode-shell/src/shell/objectdialog/render.rs` (every `writable()` site: lines ~417, 1149, 1376, 2251, 2570, 2883, 3300, 3681, 3775, 3928; `dest_label` ~2852; fold notice ~1807; field badge ~2871)
- Modify: `scripts/mutation-check.sh` (re-anchor two entries: `objectdialog: Schema is not writable` (anchor `        !matches!(self, Domain::Schema)`) and `objectdialog: a drop on the schema inspector is refused` (multi-line anchor ending `.is_some_and(|state| state.domain.writable());\n    if !writable {`); append new ones)

**Interfaces:**
- Consumes: everything Task 1 produced; `views::{kind_default, column_fields, fold_into, desk_baseline, width_text, AUTO}`.
- Produces (all in `mod.rs` unless noted):
  ```rust
  pub enum Destination { Doc, Presentation, DatasetPresentation }
  impl Domain { pub fn writable(self, stage: &Stage) -> bool }
  pub enum Provenance { Desk, Dataset, View }          // + fn name(self) -> &'static str: "desk"/"dataset"/"view"
  pub enum ColumnDoor { View, Dataset }
  #[derive(Debug, Clone, Default, PartialEq)]
  pub struct ColumnLayers { pub desk: ColumnPresentation, pub dataset: ColumnPresentation, pub view: ColumnPresentation }
  #[derive(Debug, Clone, PartialEq)]
  pub struct ColumnContext { pub door: ColumnDoor, pub layers: ColumnLayers, pub overlay_object: toml::Table, pub item: Option<ListItem> }
  pub struct Draft { .., pub column_ctx: Option<ColumnContext> }
  pub enum FellTo { Desk, Dataset, EachView }
  pub struct Fold { pub key: &'static str, pub to: Option<FellTo> }
  impl Draft { pub fn fold_column(&mut self) -> Option<Fold> }
  ```
  `views::column_fields(item: &ListItem, colours: &[String], dest: Destination) -> Vec<Field>` (the third argument is new; every field takes it); `views::fold_into(item, fields, baseline) -> Option<&'static str>` now names the key the trader CLEARED this fold (label emptied / width `auto`) whether or not `baseline` sets it; `views::baseline_below` is Task 5's.
  `views` helpers made `pub(super)`: `negative_key`, `scale_key`, `colour_key`, `width_value`, `scale_from_key`, `negative_from_key`, `colour_from_key`, `schema_role_kind`, `AUTO`, `text_row`, `choice_row`.

- [ ] **Step 1: `Destination::DatasetPresentation`.** In `mod.rs` add the variant with the doc comment `/// \`dataset_presentation.toml\`, user layer — the Schema dialog's column stage (dataset-presentation spec §4.1).` Extend `Destination::doc(self, domain)`:

```rust
            (Destination::DatasetPresentation, Domain::Schema) => {
                geode_core::view::DATASET_PRESENTATION_DOC
            }
            (Destination::DatasetPresentation, _) => {
                unreachable!("only the Schema door builds DatasetPresentation-destined fields")
            }
```

In `apply.rs` `object_value`: `Destination::Presentation | Destination::DatasetPresentation => ObjectWrite::Remove,`. In `render.rs` ~2852: `Destination::DatasetPresentation => "dataset",`. Run `cargo check -p geode-shell` and fix every exhaustive match the compiler names (there is at least the `dest_label` one; `dialog.rs` may have a footer label).

- [ ] **Step 2: Failing test for the stage-aware gate**, in `mod.rs` `mod tests`:

```rust
    #[test]
    fn schema_is_writable_in_the_column_stage_alone() {
        let column = Stage::Column {
            object: "risk".into(),
            column: "npv".into(),
        };
        for stage in [
            Stage::Browse,
            Stage::Naming,
            Stage::Edit {
                object: "risk".into(),
            },
        ] {
            assert!(!Domain::Schema.writable(&stage), "{stage:?}");
            assert!(Domain::Views.writable(&stage), "{stage:?}");
        }
        assert!(Domain::Schema.writable(&column));
        assert!(Domain::Views.writable(&column));
    }
```

Run: `cargo test -p geode-shell schema_is_writable_in_the_column_stage_alone 2>&1 | grep -E "^error|test result" | head -2` → compile error (arity).

- [ ] **Step 3: Implement `writable(stage)`**:

```rust
    /// `false` for [`Domain::Schema`] outside its column stage (§19.4,
    /// dataset-presentation spec §4.2): the create gate, the footer hints
    /// and every mutating verb read this, so a read-only surface refuses
    /// in one place. Schema's ONE writable surface is `Stage::Column`,
    /// whose fields write the dataset overlay, never the datasets doc.
    pub fn writable(self, stage: &Stage) -> bool {
        !matches!(self, Domain::Schema) || matches!(stage, Stage::Column { .. })
    }
```

Then `cargo check -p geode-shell --all-targets` and fix every call site by passing the stage: inside `render.rs` most sites have `state` in scope (`state.domain.writable(&state.stage)`); the three closure forms become `.is_some_and(|state| state.domain.writable(&state.stage))`; in `edit_commit_notice` (~1376) `domain` is copied out — change to read `let (domain, stage) = (state.domain, state.stage.clone());` and pass `&stage`; in `dialog.rs`/footers, pass the stage the same way. The compiler is the checklist: do not stop until `--all-targets` is clean. Run the gate test → `ok`.

- [ ] **Step 4: Re-anchor the two moved harness entries** in `scripts/mutation-check.sh`:
  - `objectdialog: Schema is not writable`: anchor `        !matches!(self, Domain::Schema) || matches!(stage, Stage::Column { .. })`, mutation `        true`.
  - `objectdialog: a drop on the schema inspector is refused`: in both the anchor and the mutation, replace `state.domain.writable()` with `state.domain.writable(&state.stage)`.
  Run `--anchors-only` → clean (do this before the commit at Step 12).

- [ ] **Step 5: Schema's text doors route to the Views vocabulary.** In `Domain::text_editable`: move `Domain::Schema` out of the `false` arm into `Domain::Views | Domain::Schema => views::text_editable(key),` (its only `Text` rows outside the column stage are keyed `columns.<name>` / `derived.<name>`, which `views::text_editable` answers `false` for — it matches `label | width` only). Same for `parse_text`: `Domain::Views | Domain::Schema => views::parse_text(key, text),`. Add one test:

```rust
    #[test]
    fn schema_types_only_into_the_column_stages_label_and_width() {
        assert!(Domain::Schema.text_editable("label"));
        assert!(Domain::Schema.text_editable("width"));
        assert!(!Domain::Schema.text_editable("columns.npv"));
        assert!(!Domain::Schema.text_editable("derived.region"));
    }
```

- [ ] **Step 6: `Provenance`, `ColumnDoor`, `ColumnLayers`, `ColumnContext`, `Draft.column_ctx`.** Add to `mod.rs` beside `Field`:

```rust
/// Which layer's value a column-stage field is showing (dataset-
/// presentation spec §5.3): the trader's own view-level override, their
/// dataset-level setting, or the desk view's own key. `None` when the
/// kind default is in force. Painted as a lowercase chip in the slot the
/// Schema rows' layer badge uses — the two never appear together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    Desk,
    Dataset,
    View,
}

impl Provenance {
    pub fn name(self) -> &'static str {
        match self {
            Provenance::Desk => "desk",
            Provenance::Dataset => "dataset",
            Provenance::View => "view",
        }
    }
}

/// Which door opened the column stage (§4.1, §5): the Views dialog's
/// member row (the fields write the view overlay over a desk + dataset
/// baseline) or the Schema dialog's column row (the fields write the
/// dataset overlay over the kind default).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnDoor {
    View,
    Dataset,
}

/// The three layers under one column, each as the keys that layer
/// itself sets — NOT merged — so provenance and the fold can name a
/// layer (§5.1–§5.3).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ColumnLayers {
    pub desk: ColumnPresentation,
    pub dataset: ColumnPresentation,
    pub view: ColumnPresentation,
}

impl ColumnLayers {
    /// desk with the dataset level merged over — what a cleared VIEW key
    /// falls to, and what the view writer compares against (§5.1).
    pub fn below_view(&self) -> ColumnPresentation {
        let mut p = self.desk.clone();
        p.merge_over(&self.dataset);
        p
    }
}

/// Everything a column stage needs beyond the seven fields, set by the
/// door that opened it and dropped by `leave_column`.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnContext {
    pub door: ColumnDoor,
    pub layers: ColumnLayers,
    /// The Schema door only: the `[<dataset>]` table of
    /// `dataset_presentation.toml` as it stands (empty when absent), so
    /// the writer can render the dataset's OTHER personalised columns
    /// verbatim beside the one being edited (§4.5).
    pub overlay_object: toml::Table,
    /// The Schema door only: the scratch item the fields fold into,
    /// where the Views door folds into its parent list's item.
    pub item: Option<ListItem>,
}
```

Add `pub column_ctx: Option<ColumnContext>,` to `Draft` (after `column`), initialise it `None` in `Domain::draft`, `Draft::new_object` and every other `Draft { .. }` literal the compiler names; clear it in `leave_column` (`self.column_ctx = None;` next to `self.column.take()`).

- [ ] **Step 7: `enter_column` accepts a Schema column row.** Replace the `is_member` computation:

```rust
        // Membership is what the door lists: the Views door's `columns`
        // list, or — the Schema door — a parent field keyed
        // `columns.<col>`, which is how `schema::fields` names a column
        // row (dataset-presentation spec §4.1).
        let listed = self
            .list_items("columns")
            .is_some_and(|items| items.iter().any(|i| i.name == column));
        let row_key = format!("columns.{column}");
        let is_member = listed || self.fields.iter().any(|f| f.key == row_key);
```

- [ ] **Step 8: `fold_into` names the cleared key regardless of the baseline.** In `views.rs` change the two clearing arms so `followed_desk` (rename the local to `cleared`) is set whenever the label is emptied or the width is `auto`, not only when `desk.label/width.is_some()`; keep the fall-to-baseline assignments. Update its doc: "Returns the key the trader CLEARED this fold — the caller decides whether that is worth a notice, from what it fell to."

- [ ] **Step 9: `fold_column` over either door, returning `Fold`.** Add to `mod.rs`:

```rust
/// What a cleared column-stage key fell to (dataset-presentation spec
/// §3.3, §4.4, §5.2): the desk view's own key, the dataset level, or —
/// from the Schema door — whatever each view says. `None` means nothing
/// below sets the key, so there is nothing to tell the trader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FellTo {
    Desk,
    Dataset,
    EachView,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fold {
    pub key: &'static str,
    pub to: Option<FellTo>,
}
```

and rewrite `fold_column`:

```rust
    pub fn fold_column(&mut self) -> Option<Fold> {
        let name = self.column.clone()?;
        let ctx = self.column_ctx.clone()?;
        let baseline = match ctx.door {
            ColumnDoor::View => ctx.layers.below_view(),
            ColumnDoor::Dataset => ColumnPresentation::default(),
        };
        let cleared = match ctx.door {
            ColumnDoor::View => {
                let parent = self.parent_fields.as_mut()?;
                let field = parent.iter_mut().find(|f| f.key == "columns")?;
                let FieldKind::OrderedList { items, .. } = &mut field.kind else {
                    return None;
                };
                let item = items.iter_mut().find(|i| i.name == name)?;
                let cleared = views::fold_into(item, &self.fields, &baseline);
                let (label, width) = (item.presentation.label.clone(), item.presentation.width);
                self.reseed_cleared_texts(label, width);
                cleared
            }
            ColumnDoor::Dataset => {
                let ctx_mut = self.column_ctx.as_mut()?;
                let item = ctx_mut.item.as_mut()?;
                let cleared = views::fold_into(item, &self.fields, &baseline);
                let (label, width) = (item.presentation.label.clone(), item.presentation.width);
                self.reseed_cleared_texts(label, width);
                cleared
            }
        };
        let key = cleared?;
        let to = match ctx.door {
            ColumnDoor::Dataset => Some(FellTo::EachView),
            ColumnDoor::View => {
                let set = |p: &ColumnPresentation| match key {
                    "label" => p.label.is_some(),
                    "width" => p.width.is_some(),
                    _ => false,
                };
                if set(&ctx.layers.dataset) {
                    Some(FellTo::Dataset)
                } else if set(&ctx.layers.desk) {
                    Some(FellTo::Desk)
                } else {
                    None
                }
            }
        };
        Some(Fold { key, to })
    }

    /// After a fold, the label and width `Text` fields show what the
    /// column now has (the baseline's value once cleared), so the desk's
    /// or dataset's value reappears on the keystroke that cleared it.
    fn reseed_cleared_texts(&mut self, label: Option<String>, width: Option<f32>) {
        for field in &mut self.fields {
            let FieldKind::Text(text) = &mut field.kind else {
                continue;
            };
            match field.key.as_str() {
                "label" => *text = label.clone().unwrap_or_default(),
                "width" => *text = views::width_text(width),
                _ => {}
            }
        }
    }
```

The Views door's `ColumnContext` is set by `enter_column_stage` in Task 5 (until then the Views stage's fold returns `None` because `column_ctx` is `None` — Task 5 Step 1 restores it; run the Views column-stage tests at the END of Task 5, not here). To keep this task's suite green in between, have `enter_column_stage` (render.rs) set a provisional context for the Views door now:

```rust
    let layers = ColumnLayers {
        desk: views::desk_baseline(draft).remove(column).unwrap_or_default(),
        ..ColumnLayers::default()
    };
    draft.column_ctx = Some(ColumnContext {
        door: ColumnDoor::View,
        layers,
        overlay_object: toml::Table::new(),
        item: None,
    });
```

placed just before `draft.enter_column(column, fields)`. (Task 5 replaces `layers` with the real three.)

- [ ] **Step 10: The fold notice names its layer.** In `render.rs` ~1800:

```rust
    let mut fold = None;
    if draft.column().is_some() {
        fold = draft.fold_column();
    }
    ...
    if let Some(Fold { key, to: Some(to) }) = fold {
        let layer = match to {
            FellTo::Desk => "the desk",
            FellTo::Dataset => "the dataset",
            FellTo::EachView => "each view",
        };
        set_notice(shell, format!("{key} follows {layer} again"));
    }
```

The existing window test asserting `follows the desk again` keeps passing (desk sets the label in its fixture).

- [ ] **Step 11: `column_fields(item, colours, dest)`.** Add the third parameter; every `Field { dest: Destination::Presentation, .. }` in it (and in `text_row`/`choice_row`, which gain a `dest` parameter too) uses `dest`. Update the one caller (`enter_column_stage`) to pass `Destination::Presentation`. Widen the helper visibilities listed under Interfaces to `pub(super)`.

- [ ] **Step 12: Provenance chip.** Add `pub provenance: Option<Provenance>` to `Field` (doc: `/// The layer whose value this column-stage field shows (§5.3); \`None\` off the column stage and when the kind default is in force. Recomputed by the painter from \`Draft::column_ctx\`, never stored stale — see \`dataset_columns::provenance_of\`.`). **Do not store it**: instead the painter computes it. In `render.rs` at the field-row badge (~2871), before the `.children(field.layer.map(..))`, add:

```rust
                        .children(provenance_chip(draft, field, theme, cx))
```

with

```rust
/// §5.3: the chip naming the layer in force on a column-stage field,
/// computed from the draft's layers at paint so a stepped field reads
/// `view` (or `dataset`, from the Schema door) on the same frame.
fn provenance_chip(
    draft: &Draft,
    field: &Field,
    theme: &gpui_component::Theme,
    cx: &App,
) -> Option<AnyElement> {
    let ctx = draft.column_ctx.as_ref()?;
    let provenance = dataset_columns::provenance_of(ctx, field)?;
    Some(dialog::badge(
        provenance.name(),
        theme.muted_foreground,
        theme.border,
        Some(format!("objectdialog-field-provenance-{}", field.key)),
        cx,
    ))
}
```

`dataset_columns::provenance_of` is Task 4's (Step 1 there); to keep this task compiling, create the new module now with only that function:

```rust
//! The Schema dialog's column stage — the dataset-level door into the
//! same seven-field stage the Views dialog has (dataset-presentation
//! spec §4). Pure: no gpui.

use super::{ColumnContext, ColumnDoor, Field, FieldKind, ListItem, Provenance};
use super::views;
use geode_core::view::ColumnPresentation;

/// Which layer's value `field` is showing (§5.3). Through the Views
/// door: `View` when the field differs from the desk + dataset baseline
/// (the trader has diverged here, whether or not the write has landed),
/// else `Dataset` / `Desk` by which layer sets the key, else `None`.
/// Through the Schema door: `Dataset` when the field differs from the
/// kind default, else `None` — there is no desk value to name (§4.3, a
/// plan-level ruling over the spec's `user` badge).
pub fn provenance_of(ctx: &ColumnContext, field: &Field) -> Option<Provenance> {
    let key = field.key.as_str();
    let set_in = |p: &ColumnPresentation| match key {
        "label" => p.label.is_some(),
        "width" => p.width.is_some(),
        "scale" => p.scale.is_some(),
        "precision" => p.precision.is_some(),
        "thousands" => p.thousands.is_some(),
        "negative" => p.negative.is_some(),
        "colour" => p.colour.is_some(),
        _ => return None,
    };
    let differs_from = |p: &ColumnPresentation, kind: &geode_core::view::ColumnFormat| {
        let effective = kind.clone().with(p);
        match (key, &field.kind) {
            ("label", FieldKind::Text(t)) => t.trim() != p.label.as_deref().unwrap_or(""),
            ("width", FieldKind::Text(t)) => t.trim() != views::width_text(p.width),
            ("scale", FieldKind::Choice { options, selected }) => {
                options.get(*selected).map(String::as_str) != Some(views::scale_key(effective.scale))
            }
            ("precision", FieldKind::Number { value, .. }) => *value != i64::from(effective.precision),
            ("thousands", FieldKind::Bool(b)) => *b != effective.thousands,
            ("negative", FieldKind::Choice { options, selected }) => {
                options.get(*selected).map(String::as_str) != Some(views::negative_key(effective.negative))
            }
            ("colour", FieldKind::Choice { options, selected }) => {
                options.get(*selected).map(String::as_str) != Some(views::colour_key(&effective.colour).as_str())
            }
            _ => false,
        }
    };
    let kind = ctx
        .item
        .as_ref()
        .map(views::kind_default)
        .unwrap_or(geode_core::view::ColumnFormat::MEASURE);
    match ctx.door {
        ColumnDoor::Dataset => {
            let unset = ColumnPresentation::default();
            (differs_from(&unset, &kind) || set_in(&ctx.layers.dataset)).then_some(Provenance::Dataset)
        }
        ColumnDoor::View => {
            let below = ctx.layers.below_view();
            if differs_from(&below, &kind) || set_in(&ctx.layers.view) {
                Some(Provenance::View)
            } else if set_in(&ctx.layers.dataset) {
                Some(Provenance::Dataset)
            } else if set_in(&ctx.layers.desk) {
                Some(Provenance::Desk)
            } else {
                None
            }
        }
    }
}
```

Register `mod dataset_columns;` in `mod.rs` beside `mod views;`. The `kind` for the Views door: `ctx.item` is `None` there — have `enter_column_stage` (Views arm) also store `item: Some(item.clone())` in the context so the kind default is right for both doors (adjust the provisional context in Step 9 accordingly).

- [ ] **Step 13: Provenance tests** in `dataset_columns.rs`'s `mod tests`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{ColumnLayers, Destination};
    use geode_core::view::{ColumnPresentation, Scale};

    fn item(p: ColumnPresentation) -> ListItem {
        ListItem {
            name: "npv".into(),
            included: true,
            presentation: p,
            kind: Some("measure".into()),
        }
    }

    fn ctx(door: ColumnDoor, layers: ColumnLayers) -> ColumnContext {
        let merged = {
            let mut p = layers.below_view();
            p.merge_over(&layers.view);
            p
        };
        ColumnContext {
            door,
            item: Some(item(merged)),
            layers,
            overlay_object: toml::Table::new(),
        }
    }

    fn field(ctx: &ColumnContext, key: &str) -> Field {
        let fields = views::column_fields(ctx.item.as_ref().unwrap(), &[], Destination::Presentation);
        fields.into_iter().find(|f| f.key == key).unwrap()
    }

    #[test]
    fn provenance_names_the_layer_whose_value_is_in_force() {
        let layers = ColumnLayers {
            desk: ColumnPresentation { label: Some("desk".into()), ..Default::default() },
            dataset: ColumnPresentation { scale: Some(Scale::Thousands), ..Default::default() },
            view: ColumnPresentation { precision: Some(4), ..Default::default() },
        };
        let c = ctx(ColumnDoor::View, layers);
        assert_eq!(provenance_of(&c, &field(&c, "label")), Some(Provenance::Desk));
        assert_eq!(provenance_of(&c, &field(&c, "scale")), Some(Provenance::Dataset));
        assert_eq!(provenance_of(&c, &field(&c, "precision")), Some(Provenance::View));
        assert_eq!(provenance_of(&c, &field(&c, "colour")), None, "kind default: no chip");
    }

    #[test]
    fn a_stepped_field_reads_view_before_its_write_lands() {
        let layers = ColumnLayers {
            dataset: ColumnPresentation { scale: Some(Scale::Thousands), ..Default::default() },
            ..Default::default()
        };
        let c = ctx(ColumnDoor::View, layers);
        let mut scale = field(&c, "scale");
        if let FieldKind::Choice { options, selected } = &mut scale.kind {
            *selected = options.iter().position(|o| o == "M").unwrap();
        }
        assert_eq!(provenance_of(&c, &scale), Some(Provenance::View));
    }

    #[test]
    fn the_dataset_door_reads_dataset_or_nothing() {
        let layers = ColumnLayers {
            dataset: ColumnPresentation { width: Some(90.0), ..Default::default() },
            ..Default::default()
        };
        let c = ctx(ColumnDoor::Dataset, layers);
        assert_eq!(provenance_of(&c, &field(&c, "width")), Some(Provenance::Dataset));
        assert_eq!(provenance_of(&c, &field(&c, "label")), None);
    }
}
```

The `"M"` option spelling for millions: confirm against `views::scale_keys()`; use whatever key `scale_key(Scale::Millions)` returns.

- [ ] **Step 14: Full checks and commit.**

Run the five checks (fmt, clippy, test workspace, bench --no-run, test-support check) and `--anchors-only`. Every existing objectdialog test must still pass — the Views column stage keeps working through the provisional context. Then:

```bash
git add -A
git commit -m "objectdialog: DatasetPresentation destination, a stage-aware writable gate, Provenance, and a ColumnContext under the column stage (dataset-presentation spec §4.1–§4.2, §5.3)"
```

- [ ] **Step 15: Harness entries** (append; commit first, then run each by name):

```bash
# dataset-presentation spec §4.2: Schema is writable in its column stage
# alone — every other stage keeps the read-only notice.
run_mutation "objectdialog: Schema's column stage is its one writable surface" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '        !matches!(self, Domain::Schema) || matches!(stage, Stage::Column { .. })' \
  '        !matches!(self, Domain::Schema) || matches!(stage, Stage::Column { .. } | Stage::Edit { .. })' \
  geode-shell \
  schema_is_writable_in_the_column_stage_alone

# §5.3: a field the trader has stepped reads `view` on the same frame,
# before its write lands — otherwise the chip lies for 250 ms.
run_mutation "objectdialog: a diverged field's provenance is the view level" \
  crates/geode-shell/src/shell/objectdialog/dataset_columns.rs \
  '            if differs_from(&below, &kind) || set_in(&ctx.layers.view) {' \
  '            if set_in(&ctx.layers.view) {' \
  geode-shell \
  a_stepped_field_reads_view_before_its_write_lands

# §5.2: the fold names the layer a cleared key fell to — dataset before
# desk, since the dataset level sits above the desk view.
run_mutation "objectdialog: a cleared view key falls to the dataset level before the desk" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '                if set(&ctx.layers.dataset) {
                    Some(FellTo::Dataset)
                } else if set(&ctx.layers.desk) {' \
  '                if set(&ctx.layers.desk) {
                    Some(FellTo::Desk)
                } else if set(&ctx.layers.dataset) {' \
  geode-shell \
  clearing_a_view_label_says_it_follows_the_dataset
```

The third entry's covering test is written in Task 5 (Step 4); add the entry in Task 5 instead if you prefer the entry and its test in one commit — either way it must exist and print `caught` before Task 6.

---

### Task 4: The Schema door

**Files:**
- Modify: `crates/geode-shell/src/shell/objectdialog/dataset_columns.rs` (add `overlay_object`, `item_for`, `table`, `row_summary`)
- Modify: `crates/geode-shell/src/shell/objectdialog/schema.rs` (`fields` summary suffix; `to_table` dispatch)
- Modify: `crates/geode-shell/src/shell/objectdialog/render.rs` (`enter_column_stage` Schema arm, `commit_selected_row`, `on_edit_row_clicked`, `leave_column_stage`)
- Test: `crates/geode-shell/src/shell/tests/objectdialog.rs` (a `services_with_schema_in_dir`-style fixture; window tests)
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Consumes: Task 3's types; `schema::describe_column`; `views::{column_fields, column_summary, kind_default, schema_role_kind, fold_into, negative_key, scale_key, colour_key, width_value}`; `apply::config_with_pending`.
- Produces (`dataset_columns.rs`):
  ```rust
  pub const DOC: &str = geode_core::view::DATASET_PRESENTATION_DOC;
  pub fn overlay_object(config: &Config, dataset: &str) -> toml::Table            // `[<dataset>]` table or empty
  pub fn item_for(dataset: &DatasetSpec, column: &str, overlay_object: &toml::Table) -> Option<ListItem>
  pub fn table(draft: &Draft) -> toml_edit::Table                                  // the whole `[<dataset>]` object
  pub fn row_summary(item: &ListItem) -> String                                    // "" or " · k · 0 dp · delta"
  ```

- [ ] **Step 1: Failing pure tests** in `dataset_columns.rs` `mod tests`:

```rust
    fn risk_schema() -> geode_core::schema::SchemaSpec {
        geode_core::schema::SchemaSpec::from_doc(&geode_core::config::merge_docs(
            "datasets",
            &[geode_core::config::LayerDoc::builtin(
                "datasets",
                "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n[risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n[risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n",
            )
            .unwrap()],
        ))
        .0
    }

    fn overlay(text: &str) -> toml::Table {
        text.parse().unwrap()
    }

    #[test]
    fn item_for_seeds_from_the_overlay_or_the_kind_default() {
        let schema = risk_schema();
        let risk = schema.dataset("risk").unwrap();
        let set = item_for(risk, "npv", &overlay("[columns.npv]\nwidth = 90\nscale = \"k\"\n")).unwrap();
        assert_eq!(set.presentation.width, Some(90.0));
        assert_eq!(set.presentation.scale, Some(Scale::Thousands));
        assert_eq!(set.kind.as_deref(), Some("measure"));
        let unset = item_for(risk, "book", &overlay("")).unwrap();
        assert_eq!(unset.presentation, ColumnPresentation::default());
        assert_eq!(unset.kind.as_deref(), Some("dimension"));
        assert!(item_for(risk, "ghost", &overlay("")).is_none());
    }

    #[test]
    fn the_writer_keeps_other_columns_verbatim_and_emits_only_keys_off_the_kind_default() {
        let schema = risk_schema();
        let risk = schema.dataset("risk").unwrap();
        let overlay_object = overlay("[columns.book]\nlabel = \"Book\"\n[columns.npv]\nwidth = 90\n");
        let item = item_for(risk, "npv", &overlay_object).unwrap();
        let fields = views::column_fields(&item, &[], Destination::DatasetPresentation);
        let mut draft = Draft::new_object("risk", fields, toml::Table::new());
        draft.column_ctx = Some(ColumnContext {
            door: ColumnDoor::Dataset,
            layers: ColumnLayers::default(),
            overlay_object,
            item: Some(item),
        });
        // Precision left at the kind default (2): not written. Width kept.
        // Thousands stepped: written.
        for f in &mut draft.fields {
            if f.key == "thousands" {
                f.kind = FieldKind::Bool(false);
            }
        }
        draft.column = Some("npv".into());
        let table = table(&draft);
        let text = table.to_string();
        assert!(text.contains("[columns.book]") && text.contains("label = \"Book\""), "{text}");
        assert!(text.contains("[columns.npv]"), "{text}");
        assert!(text.contains("width = 90"), "{text}");
        assert!(text.contains("thousands = false"), "{text}");
        assert!(!text.contains("precision"), "{text}");
    }

    #[test]
    fn an_emptied_column_leaves_the_table_and_an_empty_object_is_removed() {
        let schema = risk_schema();
        let risk = schema.dataset("risk").unwrap();
        let overlay_object = overlay("[columns.npv]\nwidth = 90\n");
        let item = item_for(risk, "npv", &overlay_object).unwrap();
        let fields = views::column_fields(&item, &[], Destination::DatasetPresentation);
        let mut draft = Draft::new_object("risk", fields, toml::Table::new());
        draft.column_ctx = Some(ColumnContext {
            door: ColumnDoor::Dataset,
            layers: ColumnLayers::default(),
            overlay_object,
            item: Some(item),
        });
        draft.column = Some("npv".into());
        for f in &mut draft.fields {
            if f.key == "width" {
                f.kind = FieldKind::Text("auto".into());
            }
        }
        let table = table(&draft);
        assert!(table.is_empty(), "{table}");
        assert!(matches!(
            super::super::apply::object_value("risk", toml_edit::Item::Table(table), Destination::DatasetPresentation),
            super::super::apply::ObjectWrite::Remove
        ));
    }

    #[test]
    fn row_summary_is_empty_without_personalisation() {
        let schema = risk_schema();
        let risk = schema.dataset("risk").unwrap();
        assert_eq!(row_summary(&item_for(risk, "npv", &overlay("")).unwrap()), "");
        let set = item_for(risk, "npv", &overlay("[columns.npv]\nscale = \"k\"\nprecision = 0\n")).unwrap();
        assert_eq!(row_summary(&set), " · k · 0 dp");
    }
```

`table(&draft)` must fold the CURRENT fields into the item first (so the writer sees stepped values) — see Step 3. `apply::ObjectWrite` may need `pub(super)` visibility; make it so.

- [ ] **Step 2: Run to see them fail** (`cargo test -p geode-shell dataset_columns::tests` → compile errors on the missing functions).

- [ ] **Step 3: Implement** in `dataset_columns.rs`:

```rust
pub const DOC: &str = geode_core::view::DATASET_PRESENTATION_DOC;

/// The `[<dataset>]` table of `dataset_presentation.toml` as the config
/// holds it (§4.5) — empty when there is none. Callers pass the
/// pending-aware config (`apply::config_with_pending`).
pub fn overlay_object(config: &Config, dataset: &str) -> toml::Table {
    config
        .doc(DOC)
        .and_then(|doc| doc.value.get(dataset))
        .and_then(|v| v.as_table())
        .cloned()
        .unwrap_or_default()
}

/// The scratch item the Schema door's fields fold into: the column's
/// kind from its schema role (`views::schema_role_kind`), its
/// presentation from the overlay table's `columns.<col>` entry if any
/// (§4.3). `None` for a column the dataset does not declare.
pub fn item_for(dataset: &DatasetSpec, column: &str, overlay_object: &toml::Table) -> Option<ListItem> {
    let spec = dataset.column(column)?;
    let mut presentation = ColumnPresentation::default();
    if let Some(ct) = overlay_object
        .get("columns")
        .and_then(|c| c.as_table())
        .and_then(|c| c.get(column))
        .and_then(|v| v.as_table())
    {
        let noop = |_: &str, _: String| {};
        presentation.parse_format_keys(ct, &noop);
        presentation.parse_column_keys(ct, false, &noop);
    }
    Some(ListItem {
        name: column.to_string(),
        included: true,
        presentation,
        kind: views::schema_role_kind(&spec.role).map(str::to_string),
    })
}

/// The whole `[<dataset>]` object (§4.5): every OTHER column's table
/// copied verbatim from the overlay, the open column re-rendered from the
/// fields with only the keys that differ from the kind default (label
/// from the column's name → absent, width from `auto` → absent), and
/// dropped when nothing differs. Empty when no column remains, which
/// `apply::object_value` turns into a removal.
pub fn table(draft: &Draft) -> toml_edit::Table {
    let mut out = toml_edit::Table::new();
    let Some(ctx) = draft.column_ctx.as_ref() else {
        return out;
    };
    let Some(open) = draft.column() else {
        return out;
    };
    let mut columns = toml_edit::Table::new();
    if let Some(existing) = ctx.overlay_object.get("columns").and_then(|c| c.as_table()) {
        for (name, value) in existing {
            if name != open {
                if let Some(t) = value.as_table() {
                    columns[name.as_str()] = toml_edit::Item::Table(super::toml_table_to_edit(t));
                }
            }
        }
    }
    if let Some(item) = ctx.item.as_ref() {
        let mut folded = item.clone();
        views::fold_into(&mut folded, &draft.fields, &ColumnPresentation::default());
        let kind = views::kind_default(&folded);
        let effective = kind.clone().with(&folded.presentation);
        let p = &folded.presentation;
        let mut t = toml_edit::Table::new();
        if effective.precision != kind.precision && let Some(v) = p.precision {
            t["precision"] = toml_edit::value(i64::from(v));
        }
        if effective.thousands != kind.thousands && let Some(v) = p.thousands {
            t["thousands"] = toml_edit::value(v);
        }
        if effective.negative != kind.negative && let Some(v) = p.negative {
            t["negative"] = toml_edit::value(views::negative_key(v));
        }
        if effective.colour != kind.colour && let Some(v) = &p.colour {
            t["colour"] = toml_edit::value(views::colour_key(v));
        }
        if effective.scale != kind.scale && let Some(v) = p.scale {
            t["scale"] = toml_edit::value(views::scale_key(v));
        }
        if let Some(v) = &p.label && !v.is_empty() {
            t["label"] = toml_edit::value(v.as_str());
        }
        if let Some(v) = p.width {
            t["width"] = views::width_value(v);
        }
        if !t.is_empty() {
            columns[open] = toml_edit::Item::Table(t);
        }
    }
    if !columns.is_empty() {
        out["columns"] = toml_edit::Item::Table(columns);
    }
    out
}

/// The Schema row's suffix (§4.7): `" · k · 0 dp · delta"` when the
/// dataset table sets anything for this column, `""` otherwise.
pub fn row_summary(item: &ListItem) -> String {
    if item.presentation == ColumnPresentation::default() {
        return String::new();
    }
    let summary = views::column_summary(&views::kind_default(item), &item.presentation);
    if summary.is_empty() {
        String::new()
    } else {
        format!(" · {summary}")
    }
}
```

Check `views::column_summary`'s output shape for `scale = k, precision = 0` in its own test (`column_summary_names_only_the_keys_in_force`) and match the expected string in `row_summary_is_empty_without_personalisation` to it. `super::toml_table_to_edit` exists in `mod.rs` (used by `schema::to_table`).

- [ ] **Step 4: Dispatch in `schema.rs`.** `fields`: after building each column row, append the summary — compute `let overlay = dataset_columns::overlay_object(config, name);` once, then per column `let suffix = dataset_columns::item_for(dataset, &column.name, &overlay).map(|i| dataset_columns::row_summary(&i)).unwrap_or_default();` and set `kind: FieldKind::Text(format!("{}{suffix}", describe_column(column)))`. `to_table`:

```rust
pub fn to_table(draft: &Draft, dest: Destination) -> toml_edit::Item {
    match dest {
        Destination::DatasetPresentation => toml_edit::Item::Table(dataset_columns::table(draft)),
        Destination::Doc | Destination::Presentation => {
            toml_edit::Item::Table(super::toml_table_to_edit(&draft.source))
        }
    }
}
```

Add a `schema.rs` unit test `a_personalised_column_row_carries_its_summary` building a `Config` with the datasets doc and a `dataset_presentation` builtin doc (`[risk.columns.npv]\nscale = "k"\n`) and asserting the `columns.npv` row's text ends with ` · k`.

- [ ] **Step 5: The door in `render.rs`.** Rewrite `enter_column_stage` to branch on the domain after computing `pending`/`colours`:

```rust
    let config = pending.as_ref().unwrap_or(&shell.services.config).clone();
    ...
    let (fields, ctx) = match state.domain {
        Domain::Views => {
            let Some(item) = draft.list_items("columns").and_then(|items| items.iter().find(|i| i.name == column)).cloned() else { return; };
            let layers = views::column_layers(draft, &config, &object, column); // Task 5 — until then: the Task 3 provisional layers
            (
                views::column_fields(&item, &colours, Destination::Presentation),
                ColumnContext { door: ColumnDoor::View, layers, overlay_object: toml::Table::new(), item: Some(item) },
            )
        }
        Domain::Schema => {
            let schema = config.doc("datasets").map(|d| SchemaSpec::from_doc(d).0).unwrap_or_default();
            let Some(dataset) = schema.dataset(&object) else { return; };
            let overlay_object = dataset_columns::overlay_object(&config, &object);
            let Some(item) = dataset_columns::item_for(dataset, column, &overlay_object) else { return; };
            (
                views::column_fields(&item, &colours, Destination::DatasetPresentation),
                ColumnContext {
                    door: ColumnDoor::Dataset,
                    layers: ColumnLayers { dataset: item.presentation.clone(), ..Default::default() },
                    overlay_object,
                    item: Some(item),
                },
            )
        }
        _ => return,
    };
    draft.column_ctx = Some(ctx);
    if !draft.enter_column(column, fields) {
        draft.column_ctx = None;
        return;
    }
```

`commit_selected_row`: replace the `opens_column` computation and the name extraction:

```rust
    let target = shell.object_dialog.as_ref().and_then(|state| {
        let draft = state.draft.as_ref()?;
        if draft.column().is_some() {
            return None;
        }
        match (state.domain, draft.selected_row()?) {
            (Domain::Views, row @ EditRow::Item { .. }) => Some(draft.row_label(row)),
            (Domain::Schema, EditRow::Field(i)) => draft.fields[i].key.strip_prefix("columns.").map(str::to_string),
            _ => None,
        }
    });
    match target {
        Some(name) => enter_column_stage(shell, &name, cx),
        None => edit_commit_notice(shell),
    }
```

`on_edit_row_clicked`: after `draft.selected = position;` and the scroll, if the domain is Views or Schema and the selected row is one `commit_selected_row` would open (reuse the same `target` computation by extracting it into `fn column_stage_target(shell: &ShellView) -> Option<String>`), call `enter_column_stage(shell, &name, cx)` — this is the mouse-parity rule (§4.1) and closes the 2c ledger's "member-row click only selects" minor. It must still end in `dialog::sync_dialog_text(shell, window, cx)`.

`leave_column_stage`: for `Domain::Schema`, after `draft.leave_column()`, refresh the parent row's text so the summary reflects the fold: find the field keyed `columns.<column>` and set its `FieldKind::Text` to `describe_column(..)` + `row_summary(..)` — simplest is to re-run `schema::fields(&config_with_pending_or_services, Some(&object))` and replace `draft.fields` with it, then `draft.baseline = draft.fields.clone()` via a new `Draft::reseed_fields(fields)` helper (pub(super)) so the Schema edit stage is not spuriously dirty.

- [ ] **Step 6: Window tests** in `crates/geode-shell/src/shell/tests/objectdialog.rs`. Add a fixture that gives the Schema fixture a user dir (so the write lands on disk):

```rust
fn open_risk_columns(
    cx: &mut gpui::TestAppContext,
    dir: &std::path::Path,
) -> (Entity<ShellView>, gpui::VisualTestContext) {
    let (shell, mut cx) = dialog_test_shell_in_dir(cx, services_with_schema(), dir, "config::schema");
    cx.simulate_keystrokes("enter"); // risk's column rows
    cx.run_until_parked();
    (shell, cx)
}

/// Dataset-presentation spec §4: `enter` on a schema column row opens
/// the column stage crumbed `risk › book`; `i` types a width that lands
/// under `[risk.columns.book]`; `d`/`r` are refused; the row shows the
/// summary after; the schema rows' own verbs still answer read-only.
#[gpui::test]
fn the_schema_column_row_opens_the_column_stage_and_writes_the_dataset_overlay(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_risk_columns(cx, dir.path());
    cx.simulate_keystrokes("enter"); // book
    cx.run_until_parked();
    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Column { ref object, ref column } if object == "risk" && column == "book"
    ));
    assert!(cx.debug_bounds("objectdialog-field-width").is_some());
    for key in ["d", "r"] {
        cx.simulate_keystrokes(key);
        cx.run_until_parked();
        assert_eq!(
            dialog_state(&shell, &cx, |s| s.notice.clone()),
            Some(format!("{key} is not a verb in a column's stage"))
        );
    }
    cx.simulate_keystrokes("j i"); // width
    cx.run_until_parked();
    assert_eq!(dialog_input_text(&shell, &cx), "auto");
    cx.simulate_keystrokes("backspace backspace backspace backspace");
    cx.simulate_input("160");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        cx.debug_bounds("objectdialog-field-provenance-width").is_some(),
        true,
        "the stepped field reads `dataset`"
    );
    flush_config_write(&mut cx);
    let written = std::fs::read_to_string(dir.path().join("dataset_presentation.toml")).unwrap();
    assert!(written.contains("[risk.columns.book]"), "{written}");
    assert!(written.contains("width = 160"), "{written}");
    assert!(!written.contains("hidden"), "{written}");

    cx.simulate_keystrokes("escape"); // back to the column rows
    cx.run_until_parked();
    assert!(matches!(dialog_state(&shell, &cx, |s| s.stage.clone()), objectdialog::Stage::Edit { .. }));
    let row_text = edit_draft(&shell, &cx, |d| {
        d.fields.iter().find(|f| f.key == "columns.book").map(|f| match &f.kind {
            objectdialog::FieldKind::Text(t) => t.clone(),
            _ => String::new(),
        })
    });
    assert!(row_text.as_deref().is_some_and(|t| t.ends_with("160 px")), "{row_text:?}");
    cx.simulate_keystrokes("d");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.notice.clone()).as_deref(),
        Some(objectdialog::READ_ONLY_NOTICE),
        "the schema rows' own verbs stay read-only"
    );
}

/// §4.1 mouse parity: a click on a schema column row opens the stage.
#[gpui::test]
fn a_click_on_a_schema_column_row_opens_the_column_stage(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_risk_columns(cx, dir.path());
    let bounds = cx.debug_bounds("objectdialog-field-columns.book").expect("row painted");
    cx.simulate_click(bounds.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(matches!(
        dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Column { ref column, .. } if column == "book"
    ));
}
```

Adjust the row-text summary assertion to `column_summary`'s width spelling (`160 px`); if the field selector for the width row differs (`objectdialog-field-width` is the pattern `render.rs` uses), copy it from `the_column_stages_width_is_typed_and_refused_out_of_range`. `cx.simulate_click` is the helper the Part 2c mouse-parity tests use — copy its exact form from `a_browse_row_click_opens_its_edit_stage` (search `simulate_click` in the file).

- [ ] **Step 7: Run the objectdialog suite**, fix until green: `cargo test -p geode-shell objectdialog 2>&1 | grep -E "test result|FAILED|panicked" | head`.

- [ ] **Step 8: Five checks, anchors, commit.**

```bash
git add -A
git commit -m "objectdialog(schema): the column stage as the dataset-level door — enter/click, seeding, the differing-keys writer, the row summary (dataset-presentation spec §4)"
```

- [ ] **Step 9: Harness entries** (append; commit; run each by name):

```bash
# dataset-presentation spec §4.5: the writer keeps the dataset's OTHER
# personalised columns — dropping them would erase every other column's
# settings on any one column's edit.
run_mutation "dataset_columns: the writer keeps the other columns' tables" \
  crates/geode-shell/src/shell/objectdialog/dataset_columns.rs \
  '            if name != open {' \
  '            if false {' \
  geode-shell \
  the_writer_keeps_other_columns_verbatim_and_emits_only_keys_off_the_kind_default

# §4.5: an empty object is REMOVED, never written as a bare `[risk]`.
run_mutation "objectdialog: an empty dataset-overlay object is removed" \
  crates/geode-shell/src/shell/objectdialog/apply.rs \
  '            Destination::Presentation | Destination::DatasetPresentation => ObjectWrite::Remove,' \
  '            Destination::Presentation => ObjectWrite::Remove,
            Destination::DatasetPresentation => ObjectWrite::Nothing,' \
  geode-shell \
  an_emptied_column_leaves_the_table_and_an_empty_object_is_removed

# §4.1: a schema column row is a member of the column stage by its
# `columns.<col>` key — without this arm the Schema door cannot open.
run_mutation "objectdialog: a schema column row can enter the column stage" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '        let is_member = listed || self.fields.iter().any(|f| f.key == row_key);' \
  '        let is_member = listed;' \
  geode-shell \
  the_schema_column_row_opens_the_column_stage_and_writes_the_dataset_overlay
```

---

### Task 5: The Views stage with a layer under it

**Files:**
- Modify: `crates/geode-shell/src/shell/objectdialog/views.rs` (`desk_baseline` → `baseline_below`; new `column_layers`; `presentation_table` compares against `baseline_below`)
- Modify: `crates/geode-shell/src/shell/objectdialog/render.rs` (`enter_column_stage` Views arm uses `column_layers`)
- Test: `views.rs` `mod tests`, `tests/objectdialog.rs`
- Modify: `scripts/mutation-check.sh` (re-anchor `views: a presentation save copies the desk's widths into the user's file` if its anchor line `        if item.presentation.width != desk.width {` changes; add entries)

**Interfaces:**
- Consumes: Task 3's `ColumnLayers`, `ColumnContext`; `DatasetPresentationSpec`; `apply::config_with_pending`.
- Produces (`views.rs`):
  ```rust
  pub(super) fn dataset_layer(config: &Config, view: &ViewSpec) -> BTreeMap<String, ColumnPresentation>   // this view's dataset-level entries by column (owner rule via DatasetPresentationSpec::owner_of)
  pub(super) fn dataset_layer_for(config: &Config, view_name: &str) -> BTreeMap<String, ColumnPresentation> // reloads the view by name (load_views) and calls dataset_layer; empty when the view is unknown
  pub(super) fn baseline_below(draft: &Draft) -> BTreeMap<String, ColumnPresentation>  // desk (desk_baseline) merged with draft.dataset_layer, per column
  pub(super) fn column_layers(draft: &Draft, config: &Config, view: &str, column: &str) -> ColumnLayers
  ```
  `to_table(draft, dest)` has no config, so `Draft` carries the dataset layer: **add `pub dataset_layer: BTreeMap<String, ColumnPresentation>` to `Draft`** (empty in `new_object` and for every domain but Views), filled by `Domain::draft` through `dataset_layer_for` (Views only, from the config it is given — `enter_edit_stage` already builds the draft from `config_with_pending`) and refreshed in `enter_column_stage`. `presentation_table(draft)` keeps its signature and reads `baseline_below(draft)`.

- [ ] **Step 1: Failing writer test** in `views.rs` `mod tests` (model on the existing `a_presentation_save_writes_only_what_the_trader_changed`; read it first for the fixture shape):

```rust
    #[test]
    fn a_view_field_equal_to_the_dataset_level_writes_nothing() {
        // desk: npv width 50. dataset level: npv width 140, scale k.
        // The trader steps scale to k in the VIEW stage — equal to the
        // dataset level, so nothing is written; then width to 200 — only
        // that key is written.
        let config = config_from(&[
            (Layer::Desk, "datasets", RISK_DATASETS),
            (Layer::Desk, "views", "[tree]\ndataset = \"risk\"\ngrouping = [\"book\"]\n[[tree.columns]]\nname = \"book\"\nkind = \"dimension\"\n[[tree.columns]]\nname = \"npv\"\nkind = \"measure\"\nwidth = 50\n"),
            (Layer::User, "dataset_presentation", "[risk.columns.npv]\nwidth = 140\nscale = \"k\"\n"),
        ]);
        let mut draft = Domain::Views.draft(&config, "tree");
        assert_eq!(draft.dataset_layer["npv"].width, Some(140.0));
        let item = draft.list_items("columns").unwrap().iter().find(|i| i.name == "npv").unwrap().clone();
        assert_eq!(item.presentation.width, Some(140.0), "the member row shows the effective value");
        // Simulate the stage: fields from the item, fold with scale = k (unchanged), width 200.
        let mut fields = column_fields(&item, &[], Destination::Presentation);
        for f in &mut fields {
            if f.key == "width" {
                f.kind = FieldKind::Text("200".into());
            }
        }
        let below = baseline_below(&draft).remove("npv").unwrap();
        let mut folded = item.clone();
        fold_into(&mut folded, &fields, &below);
        // Put the folded item back and render.
        if let Some(FieldKind::OrderedList { items, .. }) = draft.fields.iter_mut().find(|f| f.key == "columns").map(|f| &mut f.kind) {
            *items.iter_mut().find(|i| i.name == "npv").unwrap() = folded;
        }
        let table = presentation_table(&draft);
        let text = table.to_string();
        assert!(text.contains("width = 200"), "{text}");
        assert!(!text.contains("scale"), "equal to the dataset level: not written — {text}");
    }
```

`RISK_DATASETS` and `config_from` exist in this module's tests or in `mod.rs`'s (search; `config_from` is in `mod.rs` tests — if not visible here, build the config with `geode_core::config::Config::load(&ConfigSources { .. })` as `load.rs`'s test does).

- [ ] **Step 2: Implement.** In `views.rs`:

```rust
/// This view's dataset-level entries, by column (dataset-presentation
/// spec §5.1): the `dataset_presentation` doc's tables for whichever
/// dataset OWNS each of the view's columns (`DatasetPresentationSpec::
/// owner_of`), read from the config the caller hands in — the
/// pending-aware one on every path that reaches a stage.
pub(super) fn dataset_layer(config: &Config, view: &ViewSpec) -> BTreeMap<String, ColumnPresentation> {
    let Some(doc) = config.doc(geode_core::view::DATASET_PRESENTATION_DOC) else {
        return BTreeMap::new();
    };
    let (overlay, _) = DatasetPresentationSpec::from_doc(doc);
    let schema = config.doc("datasets").map(|d| SchemaSpec::from_doc(d).0).unwrap_or_default();
    view.columns
        .iter()
        .filter_map(|c| {
            let owner = DatasetPresentationSpec::owner_of(view, c.name(), &schema)?;
            let p = overlay.datasets.get(owner)?.get(c.name())?.clone();
            Some((c.name().to_string(), p))
        })
        .collect()
}

/// The layer a VIEW-level key sits over (§5.1): the desk's own keys
/// (`desk_baseline`) with the dataset level merged over. Both the fold
/// and the writer read this, so a view field equal to the dataset's
/// value writes nothing.
pub(super) fn baseline_below(draft: &Draft) -> BTreeMap<String, ColumnPresentation> {
    let mut below = desk_baseline(draft);
    for (col, dataset) in &draft.dataset_layer {
        below.entry(col.clone()).or_default().merge_over(dataset);
    }
    below
}

/// The three layers under one column (§5.3), for the provenance chip
/// and the fold notice. `view` is the trader's view-overlay entry as the
/// doc holds it — the fields' own values are compared against
/// `below_view()` at paint, so a just-stepped field already reads `view`.
pub(super) fn column_layers(draft: &Draft, config: &Config, view: &str, column: &str) -> ColumnLayers {
    let desk = desk_baseline(draft).remove(column).unwrap_or_default();
    let dataset = draft.dataset_layer.get(column).cloned().unwrap_or_default();
    let view_overlay = config
        .doc(PRESENTATION_DOC)
        .map(|doc| ViewPresentationSpec::from_doc(doc).0)
        .and_then(|spec| spec.views.get(view).and_then(|v| v.columns.get(column)).cloned())
        .unwrap_or_default();
    ColumnLayers { desk, dataset, view: view_overlay }
}
```

In `presentation_table` replace `let baseline = desk_baseline(draft);` with `let baseline = baseline_below(draft);` and rename the per-item local `desk` to `below` (the harness entry `views: a presentation save copies the desk's widths into the user's file` anchors on `        if item.presentation.width != desk.width {` — re-anchor it to the renamed line). In `Domain::draft` (mod.rs), after building `fields` for `Domain::Views`, fill `draft.dataset_layer` — `views::fields` already loads the views; expose `views::dataset_layer_for(config, object) -> BTreeMap<..>` that reloads the view by name, and call it from `Domain::draft` when `self == Domain::Views` (every other domain leaves the map empty). In `enter_column_stage`'s Views arm replace the provisional layers with `views::column_layers(draft, &config, &object, column)` and refresh `draft.dataset_layer = views::dataset_layer_for(&config, &object)` first, so a dataset-level edit made inside the debounce is seen.

- [ ] **Step 3: Run the writer test** → `ok`; run the whole views test module → `ok`.

- [ ] **Step 4: Window test** for the fold notice and the chip, in `tests/objectdialog.rs`, using `desk_view_services(&[("dataset_presentation", "[risk_snapshot.columns.npv]\nlabel = \"NPV k\"\nscale = \"k\"\n")])` (the fixture helper already accepts extra user-layer docs) through a `dialog_test_shell_in_dir` opener like `open_tree_edit_stage`:

```rust
/// Dataset-presentation spec §5: with a dataset-level label under it,
/// clearing the view's label says it follows the dataset; the chip reads
/// `dataset` for scale (set there), `desk` for nothing here, and `view`
/// once a field is stepped.
#[gpui::test]
fn clearing_a_view_label_says_it_follows_the_dataset(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let services = desk_view_services(&[(
        "dataset_presentation",
        "[risk_snapshot.columns.npv]\nlabel = \"NPV k\"\nscale = \"k\"\n",
    )]);
    let (shell, mut cx) = dialog_test_shell_in_dir(cx, services, dir.path(), "config::views");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.simulate_keystrokes("j j enter"); // npv's column stage
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-field-provenance-scale").is_some(), "scale is set at the dataset level");
    cx.simulate_keystrokes("i"); // label
    cx.run_until_parked();
    assert_eq!(dialog_input_text(&shell, &cx), "NPV k", "seeded with the dataset's label, which beats the desk's `NPV`");
    cx.simulate_keystrokes("backspace backspace backspace backspace backspace");
    cx.simulate_input("mine");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.simulate_keystrokes("i");
    cx.run_until_parked();
    cx.simulate_keystrokes("backspace backspace backspace backspace enter");
    cx.run_until_parked();
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.notice.clone()).as_deref(),
        Some("label follows the dataset again")
    );
    assert_eq!(dialog_input_text(&shell, &cx), "", "the field is closed");
    assert_eq!(
        edit_draft(&shell, &cx, |d| d.fields.iter().find(|f| f.key == "label").map(|f| match &f.kind {
            objectdialog::FieldKind::Text(t) => t.clone(),
            _ => String::new(),
        })),
        Some("NPV k".into()),
        "re-seeded from the dataset level"
    );
    flush_config_write(&mut cx);
    let written = std::fs::read_to_string(dir.path().join("view_presentation.toml")).unwrap_or_default();
    assert!(!written.contains("label"), "a cleared key is not written — {written}");
}
```

Check the exact keystroke sequence to reach npv's column stage against `open_tree_edit_stage` + `the_column_stages_width_is_typed_and_refused_out_of_range` (they use `j enter` after the opener's `j j`); mirror it.

- [ ] **Step 5: Run, five checks, anchors, commit.**

```bash
git add -A
git commit -m "objectdialog(views): the column stage's baseline is desk + dataset; the fold names its layer; provenance per field (dataset-presentation spec §5)"
```

- [ ] **Step 6: Harness entries** (append; commit; run by name — include Task 3's third entry here if it was deferred):

```bash
# dataset-presentation spec §5.1: the view writer compares against desk +
# dataset, so a view field equal to the dataset level writes NOTHING —
# compared against the desk alone, every dataset-level key would be
# copied into the view overlay as a spurious override.
run_mutation "views: the overlay writer's baseline includes the dataset level" \
  crates/geode-shell/src/shell/objectdialog/views.rs \
  '    let baseline = baseline_below(draft);' \
  '    let baseline = desk_baseline(draft);' \
  geode-shell \
  a_view_field_equal_to_the_dataset_level_writes_nothing

# §5.1: the dataset layer is looked up by the column's OWNER — the join's
# dataset for a joined column, never only the view's own.
run_mutation "views: the dataset layer follows the column's owning dataset" \
  crates/geode-shell/src/shell/objectdialog/views.rs \
  '            let owner = DatasetPresentationSpec::owner_of(view, c.name(), &schema)?;' \
  '            let owner = view.dataset.as_str();' \
  geode-shell \
  a_view_field_equal_to_the_dataset_level_writes_nothing
```

If the second entry survives (the fixture has no join), add a joined-view fixture to `a_view_field_equal_to_the_dataset_level_writes_nothing` or write `a_joined_columns_dataset_layer_comes_from_the_join` and name it instead — a `SURVIVED` is a test that cannot see the behaviour, never a reason to drop the entry.

---

### Task 6: Docs, harness as a set, full verification

**Files:** the spec (`## 9. As built`), `CLAUDE.md`, `scripts/mutation-check.sh` (count), the 4c spec §19.4 (one sentence: the gate is stage-aware now) and the 2c spec §9 (the deferred "member-row click only selects" minor is closed).

- [ ] **Step 1: `## 9. As built`** in the dataset-presentation spec, in the 2c spec's §9 shape: per task what shipped; the two plan-level rulings (the `dataset` chip over the spec's `user` badge, §4.3; the Views member-row click opening the column stage, §4.1's parity handler); every deviation with its reason; the deferred minors the task reviews collected; the display-pending list (the provenance chip, the Schema row summary, the crumb in the Schema stage); the harness count from `--anchors-only`.
- [ ] **Step 2: `CLAUDE.md`.** One paragraph `**Dataset-level column presentation is done**` after the `**Phase 4c Part 2c is done**` paragraph: the doc and its resolution order, `DatasetPresentationSpec::{from_doc, apply, owner_of}`, the merge point in `load_views` (between desk keys and the view overlay — the maintainer trap: a reader that applies it after the view overlay silently flips who wins), `Domain::writable(stage)` with Schema writable in `Stage::Column` alone, `ColumnContext`/`ColumnDoor` so one stage serves two doors, `Provenance` computed at paint, `baseline_below` and why a view field equal to the dataset value writes nothing, the fold notice's three spellings. Update the harness count on the command line.
- [ ] **Step 3: 4c spec §19.4** — add one sentence that `writable()` became `writable(stage)` and Schema's column stage is its one writable surface (pointer to the new spec). 2c spec §9.8 — mark the "member-row click only selects" deferred minor closed by this branch.
- [ ] **Step 4: Verification** — the five CI checks, `--anchors-only`, `git status --porcelain` empty. Do NOT run the full harness; the controller runs `zsh scripts/mutation-check.sh --changed=main` detached afterwards and expects `0 SURVIVED`.
- [ ] **Step 5: Commit** — `git commit -m "docs: dataset-level column presentation as built, CLAUDE.md, harness count"`. Then `superpowers:finishing-a-development-branch` (controller).
