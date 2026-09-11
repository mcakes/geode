# Phase 4c, Part 2 Refinement — Fixed Slots, Create, Edit Filter, Visual Refresh

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the four gaps a display found in Part 2a's object dialog — nine permanent grouping rows, a way to create an object, a filterable edit stage, and the mock's chrome instead of a terminal look — before Part 2b builds Sources and the schema inspector.

**Architecture:** `shell::objectdialog` stays one pure core (`mod.rs`) plus one gpui shell (`render.rs`) over three `Domain` adapters. This plan widens the pure core in four places — a fixed roster on `Domain`, a `member` bit on `ListItem`, a `query` on `Draft`, and a `Naming` stage — and adds one write door beside `commit_edit`/`commit_removal` (`commit_create`). Chrome changes land once in `shell::dialog` (a title-row slot, a `badge` helper, a filter placeholder) so the keybindings dialog inherits them.

**Tech Stack:** Rust, gpui + gpui-component (pinned rev `0e2fb7a`), `toml_edit`, `scripts/mutation-check.sh`.

**Spec:** `docs/superpowers/specs/2026-09-08-geode-phase-4c-config-dialogs-design.md` — **§18 is this plan's whole brief**; §3 (scaffold), §4.1 (destinations), §7.1 (applying), §8.1/§8.2/§8.4 (the three adapters), §16 and §17 (as built) are the background. The visual target is the "Geode Config Dialogs" artifact of 2026-09-09.
**Also binding:** `docs/superpowers/specs/2026-09-08-geode-dialog-interaction-model-design.md` (the two-mode vocabulary).

## Global Constraints

- CI runs on **macOS and Windows**: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo bench --workspace --no-run`, `cargo check -p geode-shell --features test-support --all-targets`. Run all five before every commit that touches Rust.
- **TDD**: write the failing test, run it, watch it fail for the right reason, then implement.
- **A mutation entry for every behaviour changed**, appended to `scripts/mutation-check.sh` in the `objectdialog:` block (around line 3516), each naming the test expected to catch it as its 6th argument. **Commit before mutating.** Before adding an anchor, confirm it occurs exactly once as a substring of its file (`grep -c -F '<anchor>' <file>` must print `1`). Re-check pre-existing entries in any file you touch — a refactor silently orphans them; `zsh scripts/mutation-check.sh --anchors-only` reports stale (`ANCHOR`) and duplicated (`AMBIG`) anchors in under a second and must exit 0 before every commit.
- **Run the harness DETACHED** (`nohup zsh scripts/mutation-check.sh --changed=main > /tmp/mut.log 2>&1 &`), never in a timed foreground call. Verify `git status --porcelain` is clean afterwards.
- Nothing may stall the render thread; config writes go on the background executor (they already do — every write here goes through `apply::queue_batch`).
- **Never a raw colour** — every colour from `cx.theme()` tokens. The artifact's palette is invented; only its *structure* is the target.
- `geode-shell` never depends on `geode-data`. No crate but `geode-data` opens a file or socket, except `geode-shell` writing its own config under `user_dir` through `config_write`.
- Doc comments explain WHY, densely. **A comment that contradicts the code is a defect.** Several existing comments say the edit stage does not filter, that `n` is "Part 2's", that `enter_edit_stage` is the only door and has one caller — each such sentence this plan makes false must be rewritten in the task that makes it false.
- **Renaming an existing object stays unbuilt** (spec §8.2 ruling). Nothing in this plan edits an object's name after creation.
- The dialogs are palette-only; no new default key binding to *open* one.
- **Slot `0` is not a grouping slot** (§18.4). `GroupingSlots` stays nine wide; `ctrl+0` stays `frame::slot_clear`.

## As-built vocabulary this plan builds on

```rust
// crates/geode-shell/src/shell/objectdialog/mod.rs (Part 2a, as it stands)
pub enum Domain { Views, Groupings, Scopes }
pub enum Stage { Browse, Edit { object: String } }
pub struct ObjectRow { pub name: String, pub summary: String, pub layer: Layer,
                       pub overridden: bool, pub drifted: bool }
pub enum Destination { Doc, Presentation }
pub struct ListItem { pub name: String, pub included: bool, pub width: Option<f32> }
pub enum FieldKind { Text(String), Number{..}, Bool(bool), Choice{options, selected},
                     MultiChoice{..}, OrderedList { items: Vec<ListItem> } }
pub struct Field { pub key: String, pub label: String, pub kind: FieldKind, pub dest: Destination }
pub enum EditRow { Field(usize), Item { field: usize, item: usize } }
pub enum Confirm { Delete, Revert, Fork, Overwrite { forks: bool } }
pub struct Draft { pub name, pub fields, pub source: toml::Table, baseline, baseline_source,
                   pub selected: usize, pub diagnostics, pub confirm: Option<Confirm> }
pub enum Step { Changed, Inert, Refused(String) }
pub struct ObjectDialogState { pub domain, pub stage, pub selected, pub query: String,
                               pub mode: DialogMode, pub notice: Option<String>, pub draft: Option<Draft> }
// Domain: doc() title() summary_fn() presentation_doc() objects() fields() draft() to_table() validate()
// apply.rs: object_value(), would_fork(), blocking_diagnostic(), commit_edit(), commit_removal(),
//           queue_batch(shell, edits, user_dir, delay, cx) [private], WRITE_DEBOUNCE = 250 ms
// render.rs: open(), handle_key() → handle_browse_key()/handle_edit_key(), enter_edit_stage(),
//            build()/build_edit(), action_bar(), confirm_row(), actions(), set_notice()
// dialog.rs: ShellModal { title, build, on_key }, open_shell_dialog_with_key(view, window, cx,
//            title, build, on_key, focus_filter), filter_row(input, frozen, cx), mode_pill(mode, cx),
//            render_modal(title, content, w, h, cx)
// dialogmode.rs: NormalCommand { Nav, EnterFilter, Commit, Toggle, ToggleBack, MoveItem(i32), EditText, Verb(char) }
// listfilter.rs: rank(texts, query) -> Vec<Ranked { row, indices }>
// tests: crates/geode-shell/src/shell/tests/objectdialog.rs — dialog_test_shell_in_dir(cx, services, dir, action),
//        dialog_state(), edit_draft(), flush_config_write(), cx.simulate_keystrokes("j j"), cx.debug_bounds(selector)
```

The `Frame` is read by this dialog in exactly one place today (`render::run_confirmed`'s `Confirm::Overwrite` arm). Task 5 adds a second, for the same reason and in the same file.

---

## File map

| File | Responsibility after this plan |
|---|---|
| `crates/geode-core/src/config/mod.rs` | + `check_object_name` — the one rule for a name a layered doc can hold (Task 2) |
| `crates/geode-shell/src/frame.rs` | `save_scope` calls `check_object_name` instead of its inline rule (Task 2) |
| `crates/geode-shell/src/shell/objectdialog/mod.rs` | + `Domain::roster`, `ObjectRow.layer: Option<Layer>` (T1); `Stage::Naming`, `Draft::is_new`, `Draft::new_object`, `Domain::new_draft`, `ObjectDialogState::{begin_naming, cancel_naming}` (T3); `ListItem.member`, `Draft::remove_selected`, member-aware `step_selected`/`move_item`/`membership_changed` (T4); `Draft::query`, `Draft::visible_rows`, `Draft::row_label`, visible-aware `selected_row`/`move_item` (T6) |
| `crates/geode-shell/src/shell/objectdialog/apply.rs` | + `commit_create` (T3); `would_fork` reads `Option<Layer>` (T1) |
| `crates/geode-shell/src/shell/objectdialog/groupings.rs` | `ListItem.member: true` (T4); nothing else — the roster lives on `Domain` |
| `crates/geode-shell/src/shell/objectdialog/views.rs` | `fields` lists the dataset's other columns as non-members; `columns_for`/`presentation_table`/`doc_baseline` read members only; a new view picks the first real dataset (T3, T4) |
| `crates/geode-shell/src/shell/objectdialog/scopes.rs` | `overwrite_with` reused by create; `scope_table_as_toml` unchanged (T5) |
| `crates/geode-shell/src/shell/objectdialog/render.rs` | `n`, the naming row, create → edit (T5); edit-stage filter mode (T6); crumb + pill via `title_extra`, badges, grip/tick, section headers, outlined buttons, `x` (T8) |
| `crates/geode-shell/src/shell/dialog.rs` | `ShellModal.title_extra`, `set_title_extra`, `badge`, filter placeholder, `name_row` (T7) |
| `crates/geode-shell/src/shell/render.rs` | passes `title_extra` through to `render_modal` (T7) |
| `crates/geode-shell/src/shell/keybindings_view.rs` | pill moves into the title row through `set_title_extra` (T7) |
| `crates/geode-shell/src/shell/tests/objectdialog.rs` | window tests per task |
| `scripts/mutation-check.sh` | entries per task |
| spec §18, `CLAUDE.md` | as-built notes (T9) |

---

### Task 1: Nine fixed grouping rows, and a layer that can be absent

**Files:**
- Modify: `crates/geode-shell/src/shell/objectdialog/mod.rs` (`ObjectRow`, `Domain::objects`, `derive_rows`, tests)
- Modify: `crates/geode-shell/src/shell/objectdialog/render.rs` (every `row.layer` site: browse markers ~line 1265, edit header ~1470, `refuse_step`, `arm_delete`, `arm_overwrite`, `actions`)
- Modify: `crates/geode-shell/src/shell/objectdialog/apply.rs` (`would_fork`)
- Modify: `crates/geode-shell/src/shell/tests/objectdialog.rs`
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Produces: `ObjectRow.layer: Option<Layer>` (`None` = no layer defines this object; it exists only because the domain's roster names it). `Domain::roster(self) -> Option<&'static [&'static str]>` (`Some(["1",…,"9"])` for `Groupings`, `None` elsewhere). Later tasks read `row.layer == Some(Layer::User)` where they used to read `row.layer == Layer::User`.

- [ ] **Step 1: Write the failing pure test** in `mod.rs`'s `tests` module (it already has `config_from(&[(Layer, doc, text)]) -> Config`):

```rust
#[test]
fn groupings_always_lists_nine_slots_and_an_unconfigured_one_has_no_layer() {
    let config = config_from(&[(Layer::Desk, "groupings", "3 = [\"book\"]\n")]);
    let rows = Domain::Groupings.objects(&config);
    let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["1", "2", "3", "4", "5", "6", "7", "8", "9"]);
    let three = &rows[2];
    assert_eq!(three.layer, Some(Layer::Desk));
    assert_eq!(three.summary, "book");
    let one = &rows[0];
    assert_eq!(one.layer, None, "nothing defines slot 1");
    assert_eq!(one.summary, "empty");
    assert!(!one.overridden);
}

#[test]
fn views_have_no_roster_so_a_config_with_no_views_lists_nothing() {
    let config = config_from(&[]);
    assert!(Domain::Views.objects(&config).is_empty());
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p geode-shell groupings_always_lists_nine_slots -- --nocapture`
Expected: compile error — `Option<Layer>` vs `Layer`, and no `roster`.

- [ ] **Step 3: Implement.** In `mod.rs`:

```rust
// ObjectRow
/// The layer whose copy takes effect, or `None` when no layer defines
/// the object at all and it is on the list only because the domain's
/// roster names it (`Domain::roster` — a grouping slot nothing has
/// filled). `Option` rather than a `configured: bool` beside a
/// placeholder `Layer`: every reader of this field decides something
/// destructive or forking on it (`arm_delete`, `would_fork`), and a
/// placeholder value would answer those questions with a lie.
pub layer: Option<Layer>,
```

```rust
impl Domain {
    /// The names a domain's browse list always shows, configured or not.
    /// `Some` only for Groupings (§18.4): the nine `ctrl+1..9` slots are
    /// a fixed keyboard, so an unfilled slot is a row that reads `empty`
    /// rather than a row that does not exist — and there is no `n`,
    /// because nothing can be created that is not already on the list.
    /// Slot `0` is not here: `ctrl+0` is `frame::slot_clear`, the view's
    /// own grouping, and `GroupingSlots` is nine wide (user ruling
    /// 2026-09-10).
    fn roster(self) -> Option<&'static [&'static str]> {
        match self {
            Domain::Groupings => Some(&["1", "2", "3", "4", "5", "6", "7", "8", "9"]),
            Domain::Views | Domain::Scopes => None,
        }
    }
    pub fn objects(self, config: &Config) -> Vec<ObjectRow> {
        derive_rows(config, self.doc(), self.presentation_doc(), self.roster(), self.summary_fn())
    }
}
```

In `derive_rows`, add the `roster: Option<&'static [&'static str]>` parameter; seed `rows` **before** the layered walk so a configured slot's entry is updated in place and an unconfigured one survives untouched:

```rust
    let mut rows: BTreeMap<String, (Vec<Layer>, ObjectRow)> = BTreeMap::new();
    for name in roster.unwrap_or(&[]) {
        rows.insert(
            (*name).to_string(),
            (Vec::new(), ObjectRow {
                name: (*name).to_string(),
                summary: "empty".to_string(),
                layer: None,
                overridden: false,
                drifted: false,
            }),
        );
    }
    for layered in config.layered_docs(doc) {
        for (name, value) in &layered.table {
            if name == "config_version" { continue; }
            let entry = rows.entry(name.clone()).or_insert_with(|| (Vec::new(), ObjectRow {
                name: name.clone(), summary: String::new(), layer: Some(layered.layer),
                overridden: false, drifted: false,
            }));
            entry.0.push(layered.layer);
            entry.1.layer = Some(layered.layer);
            entry.1.summary = summary(value);
        }
    }
```

Then fix every reader. In `render.rs`: the browse-row and edit-header markers paint the layer text only `if let Some(layer) = row.layer`; `refuse_step`'s arm becomes `Some(row) if row.layer == Some(Layer::User)`; `arm_delete`'s becomes `Some(row) if row.layer == Some(Layer::User)` and its refusal message uses `row.layer.map(Layer::name).unwrap_or("no")` ("… comes from the no layer" is wrong copy — write the `None` case as its own arm: `Some(row) if row.layer.is_none() => set_notice(shell, format!("{} is empty — tick a dimension to fill it", row.name))`); `arm_overwrite`'s `forks` becomes `row.layer != Some(Layer::User)`; `actions` gates `d` on `r.layer == Some(Layer::User)`. In `apply::would_fork`: `.is_some_and(|row| row.layer.is_some_and(|layer| layer != Layer::User))` — an unconfigured slot forks nothing.

Grep for the remaining sites: `grep -rn 'layer' crates/geode-shell/src/shell/objectdialog/ crates/geode-shell/src/shell/tests/objectdialog.rs | grep -v 'Layer::\|layered\|layer_\|// '` and fix each until `cargo check -p geode-shell --features test-support --all-targets` is clean. Existing tests that build `ObjectRow { layer: Layer::User, .. }` literals become `layer: Some(Layer::User)`.

- [ ] **Step 4: Run the pure tests**

Run: `cargo test -p geode-shell objectdialog`
Expected: PASS, including the two new ones.

- [ ] **Step 5: Write the failing window test** in `tests/objectdialog.rs`, after `services_with_slot_3`:

```rust
/// §18.4: an unconfigured slot is a row, opening it shows every pickable
/// dimension unticked, and ticking the first writes the slot to the user
/// layer with NO fork question — there is no desk copy to fork.
#[gpui::test]
fn ticking_a_dimension_in_an_empty_slot_writes_it_without_asking(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx, services_with_slot_3(&["book"]), dir.path(), "config::groupings",
    );
    for slot in 1..=9 {
        assert!(cx.debug_bounds(&format!("objectdialog-row-{slot}")).is_some(), "slot {slot} row");
    }
    assert!(cx.debug_bounds("objectdialog-layer-1").is_none(), "an empty slot wears no layer");
    assert!(cx.debug_bounds("objectdialog-layer-3").is_some(), "a configured slot does");

    // Row 1 is selected on open. Open it, skip the read-only `Slot`
    // field, tick the first dimension.
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.simulate_keystrokes("j space");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-confirm").is_none(), "nothing to fork");
    let chain = shell.read_with(&cx, |shell, _| {
        shell.services.config.doc("groupings")
            .and_then(|doc| doc.value.get("1")).cloned()
    });
    assert!(chain.is_some(), "slot 1 is in the live config on the keystroke");
    flush_config_write(&mut cx);
    let written = std::fs::read_to_string(dir.path().join("groupings.toml")).unwrap();
    assert!(written.contains("1 = ["), "{written}");
}

#[gpui::test]
fn d_on_an_empty_slot_says_there_is_nothing_to_delete(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx, services_with_slot_3(&["book"]), dir.path(), "config::groupings",
    );
    cx.simulate_keystrokes("enter d");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-confirm").is_none());
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone()).unwrap_or_default();
    assert!(notice.contains("empty"), "{notice}");
}
```

The browse row's layer marker needs a debug selector for the first assertion: in `build`'s markers, wrap the layer text in `.debug_selector(move || format!("objectdialog-layer-{name}"))` (clone `row.name` before the closure, exactly as `objectdialog-overridden-{name}` does).

- [ ] **Step 6: Run the window tests**

Run: `cargo test -p geode-shell --features test-support empty_slot`
Expected: PASS.

- [ ] **Step 7: Mutation entries.** Append to `scripts/mutation-check.sh` after the last `objectdialog:` entry:

```sh
# §18.4: the roster is what puts an unfilled slot on the list. Dropping it
# leaves every configured-slot test green and the empty rows gone.
run_mutation "objectdialog: groupings roster lists the nine slots" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '            Domain::Groupings => Some(&["1", "2", "3", "4", "5", "6", "7", "8", "9"]),' \
  '            Domain::Groupings => None,' \
  geode-shell \
  groupings_always_lists_nine_slots_and_an_unconfigured_one_has_no_layer

# An unconfigured slot has no layer; forking it would ask a confirm over
# nothing. `is_some_and(.. != User)` says None forks nothing.
run_mutation "objectdialog: an unconfigured slot never forks" \
  crates/geode-shell/src/shell/objectdialog/apply.rs \
  '        .is_some_and(|row| row.layer.is_some_and(|layer| layer != Layer::User))' \
  '        .is_some_and(|row| row.layer != Some(Layer::User))' \
  geode-shell \
  ticking_a_dimension_in_an_empty_slot_writes_it_without_asking
```

Verify each anchor is unique: `grep -c -F '<anchor>' <file>` → `1`. Run `zsh scripts/mutation-check.sh --anchors-only` → exit 0.

- [ ] **Step 8: Full checks and commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test -p geode-shell --features test-support
git add -A crates/geode-shell scripts/mutation-check.sh
git commit -m "feat(config): nine fixed grouping rows; ObjectRow.layer is Option (§18.4)"
```

---

### Task 2: One name rule, shared by the dialog and the frame

**Files:**
- Modify: `crates/geode-core/src/config/mod.rs` (+ `check_object_name`, tests)
- Modify: `crates/geode-shell/src/frame.rs:550-563` (`save_scope`)

**Interfaces:**
- Produces: `pub fn check_object_name(name: &str) -> Result<&str, String>` in `geode_core::config`. Returns the trimmed name. Task 5 calls it on `enter` in the naming row.

- [ ] **Step 1: Failing tests** in `crates/geode-core/src/config/mod.rs`'s test module (add one if there is none — `#[cfg(test)] mod name_tests { use super::*; ... }`):

```rust
#[test]
fn object_names_follow_the_frames_rule() {
    assert_eq!(check_object_name("  my_books "), Ok("my_books"));
    assert_eq!(check_object_name("tree-2"), Ok("tree-2"));
    for bad in ["", "   ", "config_version", "a b", "a.b", "a\"b"] {
        assert!(check_object_name(bad).is_err(), "{bad:?} must be refused");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p geode-core object_names_follow`
Expected: FAIL — `check_object_name` not found.

- [ ] **Step 3: Implement** in `geode-core/src/config/mod.rs`:

```rust
/// Whether `name` can be the top-level key of an object in a layered
/// config doc (`[name]` in `views.toml`, `name = [...]` in
/// `groupings.toml`): trimmed, non-empty, not the `config_version`
/// stamp every doc carries, and free of the three characters that would
/// make it a quoted or dotted TOML key (whitespace, `.`, `"`).
///
/// One rule, two callers — `Frame::save_scope` (`:scope save <name>`)
/// and the object dialog's `n` — so a name the command line accepts is
/// a name the dialog accepts, and vice versa. Returns the trimmed name
/// so a caller cannot check one spelling and write another.
pub fn check_object_name(name: &str) -> Result<&str, String> {
    let name = name.trim();
    if name.is_empty()
        || name == "config_version"
        || name.contains(|c: char| c.is_whitespace() || c == '.' || c == '"')
    {
        return Err(format!("'{name}' is not a usable name"));
    }
    Ok(name)
}
```

Then in `frame.rs::save_scope` replace the inline check:

```rust
    pub fn save_scope(&mut self, name: &str) -> Result<(), String> {
        let name = geode_core::config::check_object_name(name)
            .map_err(|_| format!("'{}' is not a usable scope name", name.trim()))?;
        self.saved_scopes.insert(name.to_string(), self.scope.clone());
        self.pending_scope_persist = Some((name.to_string(), self.scope.clone()));
        self.versions.saved_scopes += 1;
        Ok(())
    }
```

(The error text is kept verbatim — a test or the command line may match on it. Check with `grep -rn "not a usable scope name" crates/` and keep whatever they expect.)

- [ ] **Step 4: Run tests**

Run: `cargo test -p geode-core object_names_follow && cargo test -p geode-shell save_scope`
Expected: PASS.

- [ ] **Step 5: Mutation entry**

```sh
# The name rule is one function so the dialog and `:scope save` cannot
# disagree. Dropping the `config_version` clause lets a trader create an
# object named after the doc's own schema stamp.
run_mutation "config: check_object_name refuses config_version" \
  crates/geode-core/src/config/mod.rs \
  '        || name == "config_version"' \
  '        || false' \
  geode-core \
  object_names_follow_the_frames_rule
```

- [ ] **Step 6: Commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings
git add crates/geode-core/src/config/mod.rs crates/geode-shell/src/frame.rs scripts/mutation-check.sh
git commit -m "feat(config): check_object_name, shared by save_scope and the dialog"
```

---

### Task 3: The create path's pure core — `Naming`, `is_new`, `new_draft`, `commit_create`

**Files:**
- Modify: `crates/geode-shell/src/shell/objectdialog/mod.rs` (`Stage`, `Draft`, `Domain::new_draft`, `ObjectDialogState`)
- Modify: `crates/geode-shell/src/shell/objectdialog/views.rs:100-150` (`fields` for `object: None`)
- Modify: `crates/geode-shell/src/shell/objectdialog/apply.rs` (+ `commit_create`)
- Modify: `crates/geode-shell/src/shell/tests/objectdialog.rs`

**Interfaces:**
- Consumes: `check_object_name` (Task 2) is *not* called here — the name arrives already checked.
- Produces:
  - `Stage::Naming` — the browse list with the filter row replaced by a name field.
  - `Draft::is_new: bool` (pub), `Draft::new_object(name: &str, fields: Vec<Field>, source: toml::Table) -> Draft` — empty baselines, `is_new: true`.
  - `Domain::new_draft(self, config: &Config, name: &str) -> Draft` — Views/Groupings from `fields(config, None)`; Scopes from `fields(config, None)` too (the caller overwrites it with the frame's scope, Task 5).
  - `ObjectDialogState::begin_naming(&mut self)` / `cancel_naming(&mut self)`; `has_previous_stage()` is `true` in `Naming` (escape goes back to browse).
  - `apply::commit_create(shell, cx) -> Option<String>` — one `Destination::Doc` write of the open draft, zero debounce.

- [ ] **Step 1: Failing pure tests** in `mod.rs` tests:

```rust
#[test]
fn a_new_view_draft_picks_the_first_real_dataset_and_no_columns() {
    let config = config_from(&[(Layer::Builtin, "datasets",
        "[risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\n")]);
    let draft = Domain::Views.new_draft(&config, "mine");
    assert!(draft.is_new);
    assert_eq!(draft.name, "mine");
    assert_eq!(draft.choice("dataset"), Some("risk"), "not the empty placeholder");
    assert!(draft.list_items("columns").unwrap().iter().all(|i| !i.included));
    assert!(draft.diagnostics.iter().all(|d| d.severity != Severity::Error), "{:?}", draft.diagnostics);
    // Everything counts as a change against an empty baseline.
    assert!(draft.is_dirty());
}

#[test]
fn naming_is_a_stage_escape_can_step_back_from() {
    let mut state = ObjectDialogState::new(Domain::Views);
    assert!(!state.has_previous_stage());
    state.begin_naming();
    assert_eq!(state.stage, Stage::Naming);
    assert!(state.has_previous_stage());
    state.cancel_naming();
    assert_eq!(state.stage, Stage::Browse);
}

#[test]
fn groupings_and_views_answer_a_new_draft_but_the_roster_domain_is_never_asked() {
    // Documented, not enforced: `n` is inert on Groupings in render.
    let config = config_from(&[]);
    assert!(Domain::Groupings.new_draft(&config, "4").is_new);
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-shell a_new_view_draft_picks`
Expected: compile errors for `new_draft`, `is_new`, `Stage::Naming`, `begin_naming`.

- [ ] **Step 3: Implement the pure core.** In `mod.rs`:

```rust
pub enum Stage {
    Browse,
    /// The browse list with the filter row replaced by a *name* field
    /// (§18.2): `n` enters it, `enter` creates, `escape` returns to
    /// `Browse` with nothing written. A stage rather than a flag on
    /// `Browse` so `has_previous_stage` turns the escape ladder's third
    /// rung on by construction, the same way `Edit` did.
    Naming,
    Edit { object: String },
}
```

`Draft` gains `pub is_new: bool` (doc: "Created by `n` this session and not yet on the browse list the config derives — the edit header says `new` beside the layer until the dialog is left. Nothing else reads it: the write path treats a new object like any other Doc write."), and:

```rust
impl Draft {
    /// A draft for an object nothing defines yet. Both baselines are
    /// EMPTY, so every field reads as changed and `writes_by_destination`
    /// names every destination — which is why `apply::commit_create`
    /// builds its own single Doc edit rather than calling `edits_for`: a
    /// new view's untouched column list would otherwise also queue an
    /// empty presentation write.
    pub fn new_object(name: &str, fields: Vec<Field>, source: toml::Table) -> Draft {
        Draft {
            name: name.to_string(), fields, source,
            baseline: Vec::new(), baseline_source: toml::Table::new(),
            selected: 0, diagnostics: Vec::new(), confirm: None, is_new: true,
        }
    }
}
impl Domain {
    /// The draft `n` opens after a name is committed (§18.2): the
    /// adapter's empty-object fields — `fields(config, None)`, which
    /// every adapter already answers — over an empty source, validated
    /// once. Scopes' caller replaces the result with the frame's scope
    /// before committing (`scopes::overwrite_with`); the empty fields
    /// are still what this returns, so the pure core never reads a
    /// `Frame`.
    pub fn new_draft(self, config: &Config, name: &str) -> Draft {
        let mut draft = Draft::new_object(name, self.fields(config, None), toml::Table::new());
        draft.diagnostics = self.validate(&draft, config);
        draft
    }
    // `draft()` sets `is_new: false` in its literal.
}
impl ObjectDialogState {
    /// `n`: the browse list stays, the filter row becomes the name field.
    /// The shared `Input` holds the name exactly as it holds a query —
    /// `set_query` mirrors it into `query` — so there is no second text
    /// buffer to keep in step.
    pub fn begin_naming(&mut self) {
        self.stage = Stage::Naming;
        self.query.clear();
        self.mode = DialogMode::Filter;
        self.notice = None;
    }
    pub fn cancel_naming(&mut self) {
        self.stage = Stage::Browse;
        self.query.clear();
        self.mode = DialogMode::Normal;
        self.selected = 0;
        self.notice = None;
    }
    pub fn has_previous_stage(&self) -> bool {
        matches!(self.stage, Stage::Edit { .. } | Stage::Naming)
    }
}
```

In `views.rs::fields`, the "own dataset is always an option" block becomes:

```rust
    let current = match view {
        Some(v) => v.dataset.clone(),
        // A view that does not exist yet has no dataset to keep; it
        // starts on the schema's first (sorted) dataset rather than on
        // an empty placeholder the reader would reject with an error —
        // which would block `commit_create` before a trader could pick.
        None => options.first().cloned().unwrap_or_default(),
    };
    if !options.contains(&current) {
        options.insert(0, current.clone());
    }
```

In `apply.rs`:

```rust
/// Record a freshly named object (§18.2) — the third door onto the
/// batch beside [`commit_edit`] and [`commit_removal`].
///
/// One `Destination::Doc` write, built here rather than by `edits_for`:
/// a new draft's baselines are empty (`Draft::new_object`), so
/// `writes_by_destination` would also name `Presentation` for an
/// untouched column list and queue an empty overlay write that means
/// "remove what is not there". Gated by [`blocking_diagnostic`] like an
/// edit (a new object the reader rejects must not reach disk); flushed
/// at `Duration::ZERO` like a removal (one decided act, nothing to
/// coalesce) so the browse list the config derives shows the object on
/// the next executor tick rather than 250 ms later.
pub(super) fn commit_create(shell: &mut ShellView, cx: &mut Context<ShellView>) -> Option<String> {
    if let Some(notice) = blocking_diagnostic(shell) {
        return Some(notice);
    }
    let Some(state) = shell.object_dialog.as_ref() else { return None };
    let Some(draft) = state.draft.as_ref() else { return None };
    let domain = state.domain;
    let item = domain.to_table(draft, Destination::Doc);
    let mut edits = BTreeMap::new();
    match object_value(&draft.name, item, Destination::Doc) {
        ObjectWrite::Set(value) => {
            edits.insert((Destination::Doc.doc(domain), draft.name.clone()), Some(value));
        }
        ObjectWrite::Remove | ObjectWrite::Nothing => {
            return Some("nothing to create — the object would be empty".to_string());
        }
    }
    let Some(user_dir) = shell.user_dir.clone() else {
        return Some("no writable user config directory — nothing was changed".to_string());
    };
    if let Some(draft) = shell.object_dialog.as_mut().and_then(|s| s.draft.as_mut()) {
        draft.mark_saved();
    }
    queue_batch(shell, edits, user_dir, Duration::ZERO, cx);
    None
}
```

Rewrite `Draft`'s doc paragraph that says "There is no `is_new` flag … It arrives with the verb" — it has arrived.

- [ ] **Step 4: Run pure tests**

Run: `cargo test -p geode-shell objectdialog`
Expected: PASS (fix any `Draft { .. }` literal in tests missing `is_new`).

- [ ] **Step 5: Failing window test for `commit_create`** (drives the door directly, before the key exists):

```rust
/// `commit_create` writes exactly one Doc entry, at zero debounce, and
/// never an empty presentation table for the untouched column list.
#[gpui::test]
fn commit_create_writes_one_doc_entry_immediately(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx, services_with_a_desk_view(), dir.path(), "config::views",
    );
    shell.update(&mut cx, |shell, cx| {
        let draft = objectdialog::Domain::Views.new_draft(&shell.services.config, "mine");
        let state = shell.object_dialog.as_mut().unwrap();
        state.draft = Some(draft);
        state.stage = objectdialog::Stage::Edit { object: "mine".to_string() };
        assert_eq!(objectdialog::apply::commit_create(shell, cx), None);
    });
    cx.run_until_parked(); // no clock advance: zero debounce
    let written = std::fs::read_to_string(dir.path().join("views.toml")).unwrap();
    assert!(written.contains("[mine]") && written.contains("dataset = \"risk_snapshot\""), "{written}");
    assert!(!dir.path().join("view_presentation.toml").exists(), "no overlay write for a new object");
    let in_config = shell.read_with(&cx, |shell, _| {
        shell.services.config.doc("views").and_then(|d| d.value.get("mine")).is_some()
    });
    assert!(in_config);
}
```

`commit_create` needs to be reachable from the test: make it `pub(crate)` (the tests module is `crate::shell::tests`). Run: `cargo test -p geode-shell --features test-support commit_create_writes` → PASS.

- [ ] **Step 6: Mutation entries**

```sh
# A new view must start on a real dataset, or `commit_create` is blocked
# by the reader's own "missing dataset" error before a trader can pick.
run_mutation "objectdialog: a new view starts on the first real dataset" \
  crates/geode-shell/src/shell/objectdialog/views.rs \
  '        None => options.first().cloned().unwrap_or_default(),' \
  '        None => String::new(),' \
  geode-shell \
  a_new_view_draft_picks_the_first_real_dataset_and_no_columns

# Create flushes at zero debounce like a removal, not 250 ms like an edit.
run_mutation "objectdialog: create does not wait for the edit debounce" \
  crates/geode-shell/src/shell/objectdialog/apply.rs \
  '    queue_batch(shell, edits, user_dir, Duration::ZERO, cx);
    None
}' \
  '    queue_batch(shell, edits, user_dir, WRITE_DEBOUNCE, cx);
    None
}' \
  geode-shell \
  commit_create_writes_one_doc_entry_immediately
```

(The second anchor is three lines because `queue_batch(shell, edits, user_dir, Duration::ZERO, cx);` already occurs once in `commit_removal`; confirm the three-line form is unique with `grep -c`.)

- [ ] **Step 7: Commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test -p geode-shell --features test-support objectdialog
git add -A crates/geode-shell scripts/mutation-check.sh
git commit -m "feat(config): Stage::Naming, Draft::is_new, Domain::new_draft, apply::commit_create"
```

---

### Task 4: Views' two-section column list — members and available

**Files:**
- Modify: `crates/geode-shell/src/shell/objectdialog/mod.rs` (`ListItem`, `Draft::step_selected`, `Draft::move_item`, + `Draft::remove_selected`, `membership_changed`)
- Modify: `crates/geode-shell/src/shell/objectdialog/views.rs` (`fields`, `doc_table`, `columns_for` callers, `presentation_table`, `doc_baseline`)
- Modify: `crates/geode-shell/src/shell/objectdialog/groupings.rs` (`ListItem { member: true, .. }`)
- Modify: `crates/geode-shell/src/shell/objectdialog/render.rs` (`NormalCommand::Verb('x')`)
- Modify: `crates/geode-shell/src/shell/tests/objectdialog.rs`

**Interfaces:**
- Produces: `ListItem.member: bool`. **Invariant:** within one `OrderedList`, every `member: true` item precedes every `member: false` item. `Draft::step_selected` on a non-member *adds* it (member+included, moved to the end of the member block); `Draft::remove_selected() -> Step` on a member removes it (member=false, included=false, moved to the end of the list); `move_item` never crosses the boundary. `membership_changed` compares the sets of member names.

- [ ] **Step 1: Failing pure tests** in `views.rs` tests (it has `config_with_...` style fixtures; add one with two datasets' columns) and `mod.rs` tests:

```rust
// views.rs tests
#[test]
fn the_column_list_is_members_then_the_datasets_other_columns() {
    let config = config_from(&[
        (Layer::Builtin, "datasets",
         "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
          [risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\n\
          [risk.columns.delta01]\ntype = \"f64\"\nrole = \"measure\"\n"),
        (Layer::Builtin, "dimensions", "[desk]\nsql = \"substr(book, 1, 2)\"\n"),
        (Layer::Desk, "views", "[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"npv\"\n"),
    ]);
    let items = Domain::Views.draft(&config, "tree").list_items("columns").unwrap().to_vec();
    let shape: Vec<(&str, bool, bool)> = items.iter().map(|i| (i.name.as_str(), i.member, i.included)).collect();
    assert_eq!(shape, [("npv", true, true), ("book", false, false), ("delta01", false, false), ("desk", false, false)]);
}

#[test]
fn adding_an_available_column_is_a_doc_write_and_hiding_is_not() {
    let config = /* as above */;
    let mut draft = Domain::Views.draft(&config, "tree");
    // rows: Field(dataset)=0, Field(columns)=1, npv=2, book=3 …
    draft.selected = 2;
    assert!(draft.toggle_selected().changed()); // hide npv
    assert_eq!(draft.writes_by_destination().keys().copied().collect::<Vec<_>>(), [Destination::Presentation]);
    draft.mark_saved();
    draft.selected = 3;
    assert!(draft.toggle_selected().changed()); // add book
    let dests = draft.writes_by_destination();
    assert!(dests.contains_key(&Destination::Doc), "membership is definitional");
    let items = draft.list_items("columns").unwrap();
    assert_eq!(items[1].name, "book");
    assert!(items[1].member && items[1].included, "an added column joins the member block, shown");
    // The doc table now lists both, in source order then additions.
    let text = super::super::object_text("tree", Domain::Views.to_table(&draft, Destination::Doc));
    assert!(text.contains("name = \"npv\"") && text.contains("name = \"book\""), "{text}");
}

#[test]
fn x_removes_a_member_and_moves_it_to_the_available_block() {
    let config = /* as above */;
    let mut draft = Domain::Views.draft(&config, "tree");
    draft.selected = 2; // npv
    assert!(draft.remove_selected().changed());
    let items = draft.list_items("columns").unwrap();
    assert!(items.iter().all(|i| !i.member));
    assert_eq!(items.last().unwrap().name, "npv");
    assert!(draft.writes_by_destination().contains_key(&Destination::Doc));
}

#[test]
fn reordering_never_crosses_the_member_boundary() {
    let config = /* as above */;
    let mut draft = Domain::Views.draft(&config, "tree");
    draft.selected = 2; // npv, the only member
    assert!(!draft.move_item(1), "book is not a member; npv cannot move past it");
}

// mod.rs tests, on Groupings where every item is a member
#[test]
fn remove_selected_is_inert_where_membership_is_inclusion() {
    let config = config_from(&[(Layer::Builtin, "datasets",
        "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n"),
        (Layer::User, "groupings", "3 = [\"book\"]\n")]);
    let mut draft = Domain::Groupings.draft(&config, "3");
    draft.selected = 2;
    assert_eq!(draft.remove_selected(), Step::Inert);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p geode-shell the_column_list_is_members`
Expected: compile error on `member`.

- [ ] **Step 3: Implement.** `ListItem` gains:

```rust
    /// In the object at all (§18.2). A view's column list holds the
    /// view's own columns (`member: true`) followed by the dataset's
    /// other columns and every derived dimension (`member: false`), so a
    /// new view has something to tick. Membership is *definitional* —
    /// `Destination::Doc`, and a fork on a desk view — where `included`
    /// is presentation; keeping them apart is what lets hiding a desk
    /// column never fork it. Groupings sets this `true` everywhere: there,
    /// ticking IS membership.
    pub member: bool,
```

`Draft::step_selected`'s `EditRow::Item` arm, before the existing untick logic:

```rust
                if !items[item].member {
                    // Adding: into the member block, at its end, shown.
                    let end = items.iter().position(|i| !i.member).unwrap_or(items.len());
                    let mut entry = items.remove(item);
                    entry.member = true;
                    entry.included = true;
                    items.insert(end, entry);
                    // The cursor follows the item: item rows of one list
                    // are contiguous, so the row moves by (end - item).
                    self.selected = self.selected - item + end;
                    return Step::Changed;
                }
```

(`self.selected` is the unfiltered row index here; Task 6 re-derives it through the filter.) Add:

```rust
    /// `x`: take the item under the cursor out of the object (§18.2) —
    /// the definitional twin of `space`'s hide. Inert where a list has
    /// no separate membership (every item `member`, as Groupings builds
    /// its list): there `space` already unticks, and a second verb for
    /// the same act would be two answers to one question.
    pub fn remove_selected(&mut self) -> Step {
        let Some(EditRow::Item { field, item }) = self.selected_row() else { return Step::Inert };
        let FieldKind::OrderedList { items } = &mut self.fields[field].kind else { return Step::Inert };
        if items.iter().all(|i| i.member) { return Step::Inert; }
        if !items[item].member { return Step::Inert; }
        let mut entry = items.remove(item);
        entry.member = false;
        entry.included = false;
        items.push(entry);
        let last = items.len() - 1;
        self.selected = self.selected - item + last;
        Step::Changed
    }
```

`move_item`: after computing `target`, `if items[target].member != items[item].member { return false; }`. `membership_changed(before, field)`: compare `BTreeSet` of names **where `member`** on each side. In `views.rs`: `fields` builds members from the view (as today, `member: true`), then appends the chosen dataset's columns not already present (`schema.dataset(&current).map(|d| d.columns.iter().map(|c| c.name.clone()))`) and every derived dimension (`DerivedDimensions::from_doc(config.doc("dimensions"))` → `.all().map(|d| d.name.clone())`) as `ListItem { included: false, width: None, member: false }`, skipping names already listed. `doc_table`'s `wanted` and `doc_baseline`'s `wanted` filter `.filter(|i| i.member)`; `presentation_table` iterates `items.iter().filter(|i| i.member)` for `hidden`, `width` and `names`. `groupings::fields` sets `member: true` on both loops. In `render::handle_edit_key`, before the catch-all `Verb(letter)` arm:

```rust
        NormalCommand::Verb('x') => match draft_mut(shell).map(Draft::remove_selected) {
            Some(Step::Changed) => { revalidate(shell); commit_or_confirm(shell, cx); }
            Some(Step::Refused(reason)) => refuse_step(shell, reason),
            _ => set_notice(shell, "x removes a column from the view — here, space unticks".to_string()),
        },
```

Update `views.rs`'s module doc ("the columns in the order … the trader actually sees" → members first, then what the dataset offers) and `mod.rs`'s `ListItem` doc.

- [ ] **Step 4: Run pure tests**

Run: `cargo test -p geode-shell objectdialog`
Expected: PASS. Existing Views tests asserting exact item lists (e.g. `enter_opens_the_edit_stage_and_paints_every_column`, `views.rs`'s own three) will need the available block appended to their expectations — update them, do not weaken them.

- [ ] **Step 5: Failing window test** — adding forks a desk view with the confirm, hiding does not (the existing `hiding_a_column_writes_presentation_and_does_not_fork_the_view` covers the second half; add the first):

```rust
#[gpui::test]
fn adding_an_available_column_to_a_desk_view_asks_before_forking(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    // Fixture `tree` has book, npv; the dataset has exactly those two, so
    // the available block holds nothing. Extend the fixture: use
    // `desk_view_services` with a third dataset column via a user-layer
    // datasets doc is NOT possible (datasets is desk-owned) — instead
    // build services from `desk_view_docs()` with `delta01` added to the
    // datasets text (edit `desk_view_docs` to declare it; `tree` still
    // lists only book and npv, so delta01 is available).
    cx.simulate_keystrokes("j j"); // past book, npv → delta01
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-item-delta01").is_some());
    cx.simulate_keystrokes("space");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-confirm").is_some(), "membership forks a desk view");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    flush_config_write(&mut cx);
    let written = std::fs::read_to_string(dir.path().join("views.toml")).unwrap();
    assert!(written.contains("name = \"delta01\""), "{written}");
    let _ = shell;
}
```

Add `[risk_snapshot.columns.delta01]\ntype = \"f64\"\nrole = \"measure\"\n` to `desk_view_docs()`'s datasets text. Run: `cargo test -p geode-shell --features test-support adding_an_available_column` → PASS.

- [ ] **Step 6: Mutation entries**

```sh
# Membership is definitional: comparing ALL names (members and available
# alike) would call an add "no membership change" and route it to the
# overlay, silently never writing views.toml.
run_mutation "objectdialog: membership compares member names only" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '<the exact filter line in membership_changed, e.g. .filter(|i| i.member)>' \
  '<the same line with the filter removed>' \
  geode-shell \
  adding_an_available_column_is_a_doc_write_and_hiding_is_not

# An added column must join the member BLOCK, or `rows()` paints it under
# the Available header while the doc write lists it as a member.
run_mutation "objectdialog: an added item moves into the member block" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '                    items.insert(end, entry);' \
  '                    items.insert(item, entry);' \
  geode-shell \
  adding_an_available_column_is_a_doc_write_and_hiding_is_not

# Reordering across the boundary would put an available column among the
# members without changing its flag — painted in one block, written in none.
run_mutation "objectdialog: reorder never crosses the member boundary" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '        if items[target].member != items[item].member {' \
  '        if false {' \
  geode-shell \
  reordering_never_crosses_the_member_boundary
```

Fill the first entry's anchors from the code as written; confirm uniqueness.

- [ ] **Step 7: Commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test -p geode-shell --features test-support
git add -A crates/geode-shell scripts/mutation-check.sh
git commit -m "feat(config): Views lists the dataset's other columns; space adds, x removes (§18.2)"
```

---

### Task 5: `n` — the naming row, create, and the edit stage on a new object

**Files:**
- Modify: `crates/geode-shell/src/shell/objectdialog/render.rs` (`handle_key`, `handle_browse_key`, + `handle_naming_key`, `enter_edit_stage`, `build`, footer hints, edit header `new` badge)
- Modify: `crates/geode-shell/src/shell/dialog.rs` (+ `name_row`)
- Modify: `crates/geode-shell/src/shell/tests/objectdialog.rs`

**Interfaces:**
- Consumes: `Stage::Naming`, `begin_naming`/`cancel_naming`, `Domain::new_draft`, `apply::commit_create` (Task 3); `check_object_name` (Task 2); `scopes::overwrite_with` (existing).
- Produces: `dialog::name_row(input: &Entity<InputState>, label: &str, cx: &App) -> AnyElement`; `render::enter_edit_stage(shell, name, new: Option<Draft>, window, cx)`; `handle_naming_key`.

- [ ] **Step 1: Failing window tests**

```rust
/// §18.2, Views: `n` opens the name field; `enter` on a valid name
/// writes the object, opens its edit stage, and the browse list has it.
#[gpui::test]
fn n_creates_a_view_on_enter_and_opens_its_edit_stage(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx, services_with_a_desk_view(), dir.path(), "config::views",
    );
    cx.simulate_keystrokes("n");
    cx.run_until_parked();
    assert_eq!(dialog_state(&shell, &cx, |s| s.stage.clone()), objectdialog::Stage::Naming);
    assert!(cx.debug_bounds("dialog-name-row").is_some());
    assert!(dialog_filter_is_focused(&shell, &cx), "the name field owns the keys");
    cx.simulate_input("mine");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(matches!(dialog_state(&shell, &cx, |s| s.stage.clone()),
        objectdialog::Stage::Edit { object } if object == "mine"));
    assert!(edit_draft(&shell, &cx, |d| d.is_new));
    assert!(cx.debug_bounds("objectdialog-new-badge").is_some());
    assert_eq!(edit_draft(&shell, &cx, |d| d.choice("dataset").map(str::to_string)), Some("risk_snapshot".into()));
    // Zero debounce: on disk and in the config with no clock advance.
    let written = std::fs::read_to_string(dir.path().join("views.toml")).unwrap();
    assert!(written.contains("[mine]"), "{written}");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-row-mine").is_some(), "back in browse, the new row is there");
}

#[gpui::test]
fn n_refuses_a_name_any_layer_already_holds(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx, services_with_a_desk_view(), dir.path(), "config::views",
    );
    cx.simulate_keystrokes("n");
    cx.simulate_input("tree");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(dialog_state(&shell, &cx, |s| s.stage.clone()), objectdialog::Stage::Naming, "still naming");
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone()).unwrap_or_default();
    assert!(notice.contains("already exists"), "{notice}");
    assert!(!dir.path().join("views.toml").exists());
    // escape backs out with nothing written and the query gone.
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(dialog_state(&shell, &cx, |s| (s.stage.clone(), s.query.clone())),
        (objectdialog::Stage::Browse, String::new()));
}

#[gpui::test]
fn n_on_a_scope_saves_the_frames_current_scope(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx, services_with_a_saved_scope(), dir.path(), "config::scopes",
    );
    shell.update(&mut cx, |shell, cx| {
        shell.frame.update(cx, |f, _| { f.set_scope(saved_scope_books(&["BK007"])); });
    });
    cx.simulate_keystrokes("n");
    cx.simulate_input("today");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let written = std::fs::read_to_string(dir.path().join("scopes.toml")).unwrap();
    assert!(written.contains("[today") && written.contains("BK007"), "{written}");
    assert!(edit_draft(&shell, &cx, |d| d.is_new));
}

#[gpui::test]
fn n_is_inert_on_groupings_and_says_why(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(
        cx, services_with_slot_3(&["book"]), dir.path(), "config::groupings",
    );
    cx.simulate_keystrokes("n");
    cx.run_until_parked();
    assert_eq!(dialog_state(&shell, &cx, |s| s.stage.clone()), objectdialog::Stage::Browse);
    let notice = dialog_state(&shell, &cx, |s| s.notice.clone()).unwrap_or_default();
    assert!(notice.contains("slots"), "{notice}");
}
```

(`saved_scope_books` exists near line 1958 — check its exact signature and adapt; `cx.simulate_input` types into the focused input, the same call the filter tests use.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p geode-shell --features test-support n_creates_a_view`
Expected: FAIL — `n` is "claimed and dropped" today, stage stays `Browse`.

- [ ] **Step 3: Implement.** In `dialog.rs`, beside `filter_row`:

```rust
/// The name field a dialog shows while creating an object (§18.2): the
/// same shared `Input`, with a muted label (`New view · name`) where the
/// filter row has its search icon. The `Input` is the filter's — the
/// dialog's `set_query` subscription mirrors the name the way it mirrors
/// a query — so there is no second text buffer to reset or focus.
pub fn name_row(input: &Entity<InputState>, label: &str, cx: &App) -> AnyElement {
    let theme = cx.theme();
    div().w_full().border_b_1().border_color(theme.border)
        .debug_selector(|| "dialog-name-row".to_string())
        .child(
            Input::new(input).appearance(false)
                .prefix(div().text_sm().text_color(theme.muted_foreground).child(label.to_string()))
                .w_full(),
        )
        .into_any_element()
}
```

In `render.rs`: `handle_key` splits three ways —

```rust
    match shell.object_dialog.as_ref().map(|s| &s.stage) {
        Some(Stage::Edit { .. }) => handle_edit_key(shell, ks, window, cx),
        Some(Stage::Naming) => handle_naming_key(shell, ks, window, cx),
        _ => handle_browse_key(shell, ks, window, cx),
    }
```

In `handle_browse_key`'s normal-mode match, replace the `_ => {}` arm's silent handling of `n`:

```rust
            NormalCommand::Verb('n') => {
                if state.domain.roster().is_some() {
                    state.notice = Some("the slots are fixed — open one to fill it".to_string());
                } else {
                    state.begin_naming();
                    input.read(cx).focus_handle(cx).focus(window, cx);
                }
            }
```

(`roster` becomes `pub(super)`.) Add:

```rust
/// The naming stage's keys (§18.2): `escape` backs out to browse with
/// nothing written; `enter` checks the name and creates; everything
/// else is the focused `Input`'s to type. The name is `state.query` —
/// mirrored from the field by the same subscription a filter uses.
fn handle_naming_key(shell: &mut ShellView, ks: &Keystroke, window: &mut Window, cx: &mut Context<ShellView>) -> bool {
    let input = shell.dialog_input.clone();
    if let Some(state) = shell.object_dialog.as_mut() && state.notice.take().is_some() {
        cx.notify();
    }
    if ks.key == "escape" {
        if let Some(state) = shell.object_dialog.as_mut() { state.cancel_naming(); }
        input.update(cx, |i, cx| i.set_value("", window, cx));
        shell.focus_handle.focus(window, cx);
        cx.notify();
        return true;
    }
    if ks.mods == Modifiers::NONE && ks.key == "enter" {
        create_from_name(shell, window, cx);
        return true;
    }
    if ks.key == "tab" { return true; }
    false
}

/// `enter` in the naming row. The one moment a name is typed and the
/// one place it is checked: `check_object_name`'s rule (the same one
/// `:scope save` applies), then "no layer already holds it" — creating
/// over a desk object would be a fork the trader did not ask for.
fn create_from_name(shell: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    let Some(state) = shell.object_dialog.as_ref() else { return };
    let domain = state.domain;
    let name = match geode_core::config::check_object_name(&state.query) {
        Ok(name) => name.to_string(),
        Err(reason) => { set_notice(shell, reason); cx.notify(); return; }
    };
    if derive_rows(shell).iter().any(|row| row.name == name) {
        set_notice(shell, format!("'{name}' already exists — open it instead"));
        cx.notify();
        return;
    }
    let mut draft = domain.new_draft(&shell.services.config, &name);
    if domain == Domain::Scopes {
        // The frame's current scope IS the new scope (§18.2) — the same
        // read `run_confirmed`'s Overwrite arm makes, for the same reason
        // it is made here and not in the pure core.
        let scope = shell.frame.read(cx).scope().clone();
        if scope.is_empty() {
            set_notice(shell, "the frame's scope is empty — nothing to save".to_string());
            cx.notify();
            return;
        }
        scopes::overwrite_with(&mut draft, &scope);
        draft.diagnostics = domain.validate(&draft, &shell.services.config);
    }
    enter_edit_stage(shell, &name, Some(draft), window, cx);
    if let Some(notice) = apply::commit_create(shell, cx) {
        set_notice(shell, notice);
    }
    cx.notify();
}
```

`enter_edit_stage` gains `new: Option<Draft>`: when `Some`, call a new `ObjectDialogState::enter_edit_with(draft)` (sets `stage: Edit { object: draft.name }`, `draft`, clears query, `Normal`, `selected = 0`) instead of `enter_edit(config, name)`; the input clear, focus and scroll tail are unchanged. Update `open_selected`'s call to pass `None`. Rewrite `enter_edit_stage`'s doc: it now has two callers and the "one door" claim is exactly that both go through it.

`build`: in `Stage::Naming`, paint `dialog::name_row(&shell.dialog_input, &format!("New {} · name", object_word(state.domain)), cx)` where the filter row goes, and the footer's action hints read `enter` create · `escape` cancel. In normal-mode browse hints add `chip("n"), sep("new ·")` unless `roster().is_some()`. `build_edit`'s header: when `draft.is_new && editing_row(shell).is_none()`, add a muted chip `new` with selector `objectdialog-new-badge` (Task 8 restyles it as a badge; a plain muted chip is fine here).

`ObjectDialogState::set_query` must not reset `selected` while `Naming`? It does — harmless, the list below is not navigable in `Naming`. Leave it.

- [ ] **Step 4: Run the four tests**

Run: `cargo test -p geode-shell --features test-support 'n_'`
Expected: PASS.

- [ ] **Step 5: Mutation entries**

```sh
# A name a layer already holds must be refused: creating `tree` would
# fork the desk's view under a verb that never said so.
run_mutation "objectdialog: n refuses an existing name" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '    if derive_rows(shell).iter().any(|row| row.name == name) {' \
  '    if false {' \
  geode-shell \
  n_refuses_a_name_any_layer_already_holds

# Scopes' n saves the FRAME's scope, not the empty object.
run_mutation "objectdialog: n on scopes reads the frame" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '        scopes::overwrite_with(&mut draft, &scope);' \
  '        let _ = &scope;' \
  geode-shell \
  n_on_a_scope_saves_the_frames_current_scope
```

- [ ] **Step 6: Commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test -p geode-shell --features test-support
git add -A crates/geode-shell scripts/mutation-check.sh
git commit -m "feat(config): n creates an object through a committed name (§18.2)"
```

---

### Task 6: Filtering the edit stage

**Files:**
- Modify: `crates/geode-shell/src/shell/objectdialog/mod.rs` (`Draft::query`, `row_label`, `visible_rows`, `selected_row`, `move_item`, `ObjectDialogState::set_query`)
- Modify: `crates/geode-shell/src/shell/objectdialog/render.rs` (`handle_edit_key`: filter mode + ladder; `build_edit`: filter row, visible rows, highlight)
- Modify: `crates/geode-shell/src/shell/tests/objectdialog.rs`

**Interfaces:**
- Produces: `Draft::query: String` (pub); `Draft::visible_rows(&self) -> Vec<listfilter::Ranked>` (indices into `rows()`); `Draft::selected_row()` now reads through `visible_rows()`; `Draft::move_item(delta) -> Option<usize>` returns the number of hidden rows skipped (`None` = did not move). `ObjectDialogState::set_query` mirrors into an open draft.

- [ ] **Step 1: Failing pure tests** in `mod.rs` tests:

```rust
#[test]
fn the_edit_stage_filters_by_label_and_the_cursor_indexes_the_filtered_list() {
    let config = config_from(&[(Layer::Builtin, "datasets",
        "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
         [risk.columns.lhu]\ntype = \"utf8\"\nrole = \"dimension\"\n"),
        (Layer::User, "groupings", "3 = [\"book\", \"lhu\"]\n")]);
    let mut draft = Domain::Groupings.draft(&config, "3");
    draft.query = "lhu".to_string();
    let visible = draft.visible_rows();
    assert_eq!(visible.len(), 1);
    draft.selected = 0;
    assert_eq!(draft.selected_row(), Some(EditRow::Item { field: 1, item: 1 }));
    assert!(draft.toggle_selected().changed(), "the verb acts on the filtered row");
}

#[test]
fn reordering_under_a_filter_moves_past_the_hidden_rows_and_says_how_many() {
    let config = config_from(&[(Layer::Builtin, "datasets",
        "[risk.columns.a]\ntype = \"utf8\"\nrole = \"dimension\"\n\
         [risk.columns.b]\ntype = \"utf8\"\nrole = \"dimension\"\n\
         [risk.columns.c]\ntype = \"utf8\"\nrole = \"dimension\"\n"),
        (Layer::User, "groupings", "3 = [\"a\", \"b\", \"c\"]\n")]);
    let mut draft = Domain::Groupings.draft(&config, "3");
    draft.query = "a c".to_string(); // hides b (rank is fuzzy; if `a c` matches b, use a query that only matches a and c)
    draft.selected = 0; // a
    assert_eq!(draft.move_item(1), Some(1), "one hidden row skipped");
    let names: Vec<&str> = draft.list_items("dimensions").unwrap().iter().map(|i| i.name.as_str()).collect();
    assert_eq!(&names[..3], ["b", "c", "a"]);
    assert_eq!(draft.selected_row(), Some(EditRow::Item { field: 1, item: 2 }), "the cursor followed a");
}
```

(If fuzzy ranking makes `"a c"` match `b`, name the columns `alpha`, `bravo`, `charlie` and query `"al ch"` — check `listfilter::rank`'s matcher and pick a query that hides exactly the middle one.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p geode-shell the_edit_stage_filters_by_label`
Expected: compile error — no `query`/`visible_rows`.

- [ ] **Step 3: Implement** in `mod.rs`:

```rust
    /// The edit stage's filter (§18.3), mirrored from the shared `Input`
    /// by `ObjectDialogState::set_query` exactly as the browse query is.
    /// Lives on the draft rather than beside it because `selected`
    /// indexes the FILTERED list and both must move together.
    pub query: String,

    /// What the filter ranks a row by: exactly the text the row paints as
    /// its label — a field's label, an item's name — and nothing more
    /// (the browse list's own rule, for the same reason: matching text
    /// the user cannot see breaks the agreement between what ranked and
    /// what is highlighted).
    pub fn row_label(&self, row: EditRow) -> String { match row {
        EditRow::Field(i) => self.fields[i].label.clone(),
        EditRow::Item { field, item } => match &self.fields[field].kind {
            FieldKind::OrderedList { items } => items[item].name.clone(),
            _ => String::new(),
        },
    } }

    /// The rows the edit stage shows, ranked by [`crate::listfilter::rank`]
    /// over [`Draft::row_label`]; every row in natural order when the
    /// query is empty. `Ranked::row` indexes [`Draft::rows`].
    pub fn visible_rows(&self) -> Vec<crate::listfilter::Ranked> {
        let labels: Vec<String> = self.rows().into_iter().map(|r| self.row_label(r)).collect();
        crate::listfilter::rank(&labels, &self.query)
    }

    pub fn selected_row(&self) -> Option<EditRow> {
        let rows = self.rows();
        self.visible_rows().get(self.selected).and_then(|m| rows.get(m.row).copied())
    }
```

Every place in `step_selected`/`remove_selected` that set `self.selected = self.selected - item + end` (Task 4) becomes "re-find the moved item": add a private `fn follow(&mut self, row: EditRow)` that sets `self.selected` to the position of `row` in `visible_rows()` (or leaves it if hidden), and call it with the item's new `EditRow` after each move. `move_item`:

```rust
    /// `shift+j`/`shift+k`: move the item under the cursor past the next
    /// VISIBLE item in that direction within its own list and member
    /// block (§18.3) — under a filter that is what reordering means, and
    /// the count of hidden rows jumped over is returned so the notice can
    /// say so. `None` at either end of the block, or off an item.
    pub fn move_item(&mut self, delta: i32) -> Option<usize> {
        let EditRow::Item { field, item } = self.selected_row()? else { return None };
        let visible: BTreeSet<usize> = self.visible_rows().iter().map(|m| m.row).collect();
        let rows = self.rows();
        let row_of = |i: usize| rows.iter().position(|r| *r == EditRow::Item { field, item: i });
        let FieldKind::OrderedList { items } = &mut self.fields[field].kind else { return None };
        let block = items[item].member;
        let mut target = item;
        let mut skipped = 0usize;
        loop {
            let next = target.checked_add_signed(delta as isize)?;
            if next >= items.len() || items[next].member != block { return None; }
            target = next;
            if row_of(target).is_some_and(|r| visible.contains(&r)) { break; }
            skipped += 1;
        }
        let entry = items.remove(item);
        items.insert(target, entry);
        self.follow(EditRow::Item { field, item: target });
        Some(skipped)
    }
```

`ObjectDialogState::set_query`: `if let Some(draft) = self.draft.as_mut() { draft.query = query.clone(); draft.selected = 0; }` before the existing lines. `enter_edit`/`enter_edit_with` set `draft.query.clear()` (a new draft already has it empty). `Draft::new_object` and `Domain::draft` set `query: String::new()`.

In `render.rs::handle_edit_key`: replace the `EnterFilter` notice arm with `state.mode = DialogMode::Filter; input.read(cx).focus_handle(cx).focus(window, cx);`. Add a filter-mode block at the top (after the confirm block), mirroring `handle_browse_key`'s: `escape` → `Normal` + `shell.focus_handle.focus`; `listfilter::nav_command` → `draft.selected = vimnav::apply(draft.selected, draft.visible_rows().len(), cmd)` + scroll; `enter` → the same `Commit` notice normal mode gives; `tab` claimed; else `false`. The normal-mode `escape` branch now uses `dialogmode::escape_step(state.mode, draft.query.is_empty(), true)` and handles `ClearQuery` (clear `draft.query`, `set_value("")`, scroll to 0) as well as `PreviousStage`. The `MoveItem` arm reads `Some(skipped)`: `skipped > 0` → notice `format!("moved past {skipped} hidden")`. `Nav` clamps against `visible_rows().len()`.

`build_edit`: iterate `draft.visible_rows()` (position = filtered index, `edit_row = rows[m.row]`), paint labels through `highlighted_text(&label, &m.indices, theme.primary)`, and paint `dialog::filter_row(&shell.dialog_input, frozen_query, cx)` between the header and the list where `frozen_query = (state.mode == Normal).then_some(draft.query.as_str())`. Add `chip("/"), sep("filter ·")` to the normal-mode edit hints; the escape hint reads "back to the list" only when the query is empty, else "clear the filter".

Rewrite `render.rs`'s module-doc paragraph "The edit stage does **not** filter its own rows" and `ObjectDialogState::enter_edit`'s "the edit stage does not filter its own rows" to describe §18.3.

- [ ] **Step 4: Run pure tests**

Run: `cargo test -p geode-shell objectdialog`
Expected: PASS.

- [ ] **Step 5: Failing window test**

```rust
#[gpui::test]
fn slash_filters_the_edit_stage_and_escape_walks_the_full_ladder(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = open_tree_edit_stage(cx, dir.path());
    cx.simulate_keystrokes("/");
    cx.run_until_parked();
    assert!(dialog_filter_is_focused(&shell, &cx));
    cx.simulate_input("npv");
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-item-npv").is_some());
    assert!(cx.debug_bounds("objectdialog-item-book").is_none(), "hidden by the filter");
    assert!(cx.debug_bounds("objectdialog-field-dataset").is_none());
    cx.simulate_keystrokes("escape");            // leave filter, keep query
    cx.run_until_parked();
    assert_eq!(dialog_state(&shell, &cx, |s| s.mode), DialogMode::Normal);
    assert!(cx.debug_bounds("objectdialog-item-book").is_none(), "query still applied");
    cx.simulate_keystrokes("space");             // acts on npv, the filtered row
    cx.run_until_parked();
    assert!(edit_draft(&shell, &cx, |d| !d.list_items("columns").unwrap()[1].included));
    cx.simulate_keystrokes("escape");            // clear the query
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-item-book").is_some());
    cx.simulate_keystrokes("escape");            // back a stage
    cx.run_until_parked();
    assert_eq!(dialog_state(&shell, &cx, |s| s.stage.clone()), objectdialog::Stage::Browse);
}
```

Run: `cargo test -p geode-shell --features test-support slash_filters_the_edit_stage` → PASS.

- [ ] **Step 6: Mutation entries**

```sh
# The cursor indexes the FILTERED list. Reading rows()[selected] instead
# acts on whatever row happens to sit at that unfiltered index.
run_mutation "objectdialog: the edit cursor indexes the filtered rows" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '        self.visible_rows().get(self.selected).and_then(|m| rows.get(m.row).copied())' \
  '        rows.get(self.selected).copied()' \
  geode-shell \
  the_edit_stage_filters_by_label_and_the_cursor_indexes_the_filtered_list

# A reorder under a filter must land past the hidden neighbours, not swap
# with one of them (which looks like nothing happened).
run_mutation "objectdialog: reorder skips hidden rows" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '            if row_of(target).is_some_and(|r| visible.contains(&r)) { break; }' \
  '            break;' \
  geode-shell \
  reordering_under_a_filter_moves_past_the_hidden_rows_and_says_how_many

# set_query has to reach the draft, or typing narrows the browse copy and
# the edit stage keeps painting every row.
run_mutation "objectdialog: set_query mirrors into the open draft" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '            draft.query = query.clone();' \
  '            draft.query = String::new();' \
  geode-shell \
  slash_filters_the_edit_stage_and_escape_walks_the_full_ladder
```

- [ ] **Step 7: Commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test -p geode-shell --features test-support
git add -A crates/geode-shell scripts/mutation-check.sh
git commit -m "feat(config): the edit stage filters, verbs act by identity (§18.3)"
```

---

### Task 7: Shared chrome — title slot, badge, filter placeholder, pill moves

**Files:**
- Modify: `crates/geode-shell/src/shell/dialog.rs` (`ShellModal`, `open_shell_dialog_with_key`, + `set_title_extra`, + `badge`, `filter_row`, `render_modal`)
- Modify: `crates/geode-shell/src/shell/render.rs:943-946` and the `render_modal(` call ~1332
- Modify: `crates/geode-shell/src/shell/keybindings_view.rs:541` (open) and `~1545-1560` (build)
- Modify: `crates/geode-shell/src/shell/tests/keybindings.rs` (or wherever `dialog-mode-pill-normal` is asserted — `grep -rn 'dialog-mode-pill' crates/geode-shell/src/shell/tests/`)

**Interfaces:**
- Produces:
  - `pub type TitleExtraBuilder = Rc<dyn Fn(&ShellView, &mut App) -> AnyElement>`; `ShellModal.title_extra: Option<TitleExtraBuilder>`; `pub fn set_title_extra(view: &mut ShellView, build: impl Fn(&ShellView, &mut App) -> AnyElement + 'static)`.
  - `pub(crate) fn badge(label: impl Into<SharedString>, fg: Hsla, border: Hsla, selector: Option<String>, cx: &App) -> AnyElement`.
  - `filter_row` paints `press / to filter` (selector `dialog-filter-placeholder`) when `frozen == Some("")`.
  - `render_modal(title, title_extra: Option<AnyElement>, content, w, h, cx)`.

- [ ] **Step 1: Failing window test** in the keybindings test file:

```rust
/// §18.1: the mode pill lives in the modal's title row, not in the
/// dialog's content, and the frozen empty filter shows a placeholder.
#[gpui::test]
fn the_keybinding_dialogs_pill_sits_in_the_title_row(cx: &mut gpui::TestAppContext) {
    let (_shell, cx) = dialog_test_shell(cx, "keybindings::open"); // whatever the existing tests use
    let pill = cx.debug_bounds("dialog-mode-pill-normal").expect("pill paints");
    let title = cx.debug_bounds("shell-modal-title").expect("title paints");
    assert!((pill.origin.y - title.origin.y).abs() < title.size.height, "same row as the title");
    assert!(cx.debug_bounds("dialog-filter-placeholder").is_some());
}
```

Give the title `div` in `render_modal` a `.debug_selector(|| "shell-modal-title".to_string())`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p geode-shell --features test-support the_keybinding_dialogs_pill_sits`
Expected: FAIL on the y comparison (pill is below the title today) or on the placeholder.

- [ ] **Step 3: Implement.** In `dialog.rs`:

```rust
pub type TitleExtraBuilder = Rc<dyn Fn(&ShellView, &mut App) -> AnyElement>;
pub struct ShellModal {
    pub title: SharedString,
    /// What a dialog paints in the title row between the title and the
    /// close button (§18.1): a count crumb, the mode pill. Built per
    /// frame like `build`, and for the same `&ShellView` reason. `None`
    /// for a dialog with nothing to say there (settings).
    pub title_extra: Option<TitleExtraBuilder>,
    pub build: ModalBuilder,
    pub on_key: Option<ModalKeyHandler>,
}
/// Give the open modal a title-row extra. Called straight after
/// `open_shell_dialog_with_key` by the dialogs that want one, rather
/// than as an eighth parameter on a door six callers already pass
/// through — a modal without one is the common case.
pub fn set_title_extra(view: &mut ShellView, build: impl Fn(&ShellView, &mut App) -> AnyElement + 'static) {
    if let Some(modal) = view.modal.as_mut() { modal.title_extra = Some(Rc::new(build)); }
}
```

`open_shell_dialog_with_key` sets `title_extra: None`. `render_modal` gains `title_extra: Option<AnyElement>` and the title row becomes `title` · `h_flex().gap_2().items_center().children(title_extra)` · close button, with `.justify_between()` kept by wrapping the extra and the close button in one right-hand `h_flex`. In `render.rs`, the modal extraction becomes `(modal.title.clone(), modal.title_extra.clone(), modal.build.clone())` and the call builds `let extra = title_extra.map(|f| f(self, cx));` before `build`.

```rust
/// A bordered mono pill for a *classification* — a layer, `overridden`,
/// `drifted`, a field's destination (§18.1). Distinct from `key_chip`
/// (filled, for a keystroke) and `mode_pill` (filled, for a state): a
/// badge is outlined so a row wearing three of them still reads as one
/// row. `fg` colours text and `border` the outline; the fill is the
/// panel's own.
pub(crate) fn badge(label: impl Into<SharedString>, fg: Hsla, border: Hsla, selector: Option<String>, cx: &App) -> AnyElement {
    let _ = cx;
    let label = label.into();
    let mut el = div().font_family(crate::fonts::MONO).text_xs().text_color(fg)
        .border_1().border_color(border).px_1().rounded(px(3.)).flex_shrink_0().child(label);
    if let Some(selector) = selector { el = el.debug_selector(move || selector.clone()); }
    el.into_any_element()
}
```

`filter_row`'s `Some(query)` arm: `if query.is_empty()` paint the icon plus `"press / to filter"` in `muted_foreground` with `.debug_selector(|| "dialog-filter-placeholder".to_string())`. In `keybindings_view.rs`: in `open`, after `open_shell_dialog_with_key`, call `dialog::set_title_extra(view, |shell, cx| shell.keybindings.as_ref().map(|s| dialog::mode_pill(s.mode, cx)).unwrap_or_else(|| div().into_any_element()))`; in `build`, delete the pill `h_flex` and its comment (which says the pill *cannot* live in the title row — now false). Do the same removal in `objectdialog/render.rs::build` (~line 1420) and add the same `set_title_extra` in `objectdialog::render::open` for now (Task 8 replaces it with crumb + pill).

- [ ] **Step 4: Run all shell tests**

Run: `cargo test -p geode-shell --features test-support`
Expected: PASS. Any test asserting the pill's position relative to the filter row must be updated to the title row.

- [ ] **Step 5: Mutation entry**

```sh
# The placeholder is what tells a trader in normal mode that `/` exists.
run_mutation "dialog: the frozen empty filter shows its placeholder" \
  crates/geode-shell/src/shell/dialog.rs \
  '"press / to filter"' \
  '""' \
  geode-shell \
  the_keybinding_dialogs_pill_sits_in_the_title_row
```

- [ ] **Step 6: Commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings
git add -A crates/geode-shell scripts/mutation-check.sh
git commit -m "feat(shell): modal title slot, badge helper, filter placeholder; pill moves to the title row (§18.1)"
```

---

### Task 8: The object dialog to the mock

**Files:**
- Modify: `crates/geode-shell/src/shell/objectdialog/render.rs` (`open`, `build`, `build_edit`, `action_bar`, footer hints)
- Modify: `crates/geode-shell/src/shell/objectdialog/mod.rs` (+ `Domain::crumb_noun`)
- Modify: `crates/geode-shell/src/shell/tests/objectdialog.rs`

**Interfaces:**
- Consumes: `dialog::set_title_extra`, `dialog::badge`, `dialog::mode_pill` (Task 7); `ListItem.member` (Task 4); `Draft::is_new` (Task 3).
- Produces: selectors `objectdialog-crumb`, `objectdialog-layer-{name}` (Task 1's, now a badge), `objectdialog-dest-{key}`, `objectdialog-section-members-{key}`, `objectdialog-section-available-{key}`, `objectdialog-new-badge`.

- [ ] **Step 1: Failing window test**

```rust
#[gpui::test]
fn the_edit_stage_paints_section_headers_destination_badges_and_the_crumb(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_shell, cx) = open_tree_edit_stage(cx, dir.path());
    assert!(cx.debug_bounds("objectdialog-section-members-columns").is_some());
    assert!(cx.debug_bounds("objectdialog-section-available-columns").is_some(), "delta01 is available");
    assert!(cx.debug_bounds("objectdialog-dest-dataset").is_some());
    assert!(cx.debug_bounds("objectdialog-dest-columns").is_some());
    assert!(cx.debug_bounds("dialog-mode-pill-normal").is_some());
}

#[gpui::test]
fn the_browse_crumb_counts_and_a_slot_crumb_names_its_chord(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(cx, services_with_slot_3(&["book"]), dir.path(), "config::groupings");
    let crumb = shell.read_with(&cx, |shell, _| objectdialog::render::crumb_text(shell));
    assert_eq!(crumb, "9 slots");
    cx.simulate_keystrokes("j j enter");
    cx.run_until_parked();
    let crumb = shell.read_with(&cx, |shell, _| objectdialog::render::crumb_text(shell));
    assert_eq!(crumb, "ctrl+3");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p geode-shell --features test-support section_headers`
Expected: FAIL — selectors absent, `crumb_text` missing.

- [ ] **Step 3: Implement.** In `mod.rs`:

```rust
impl Domain {
    /// The plural noun the browse crumb counts (`2 views`, `9 slots`,
    /// `3 saved`) — not `title()`, whose "Groupings" would count the
    /// wrong thing.
    pub fn crumb_noun(self) -> &'static str {
        match self { Domain::Views => "views", Domain::Groupings => "slots", Domain::Scopes => "saved" }
    }
}
```

In `render.rs`:

```rust
/// The title-row crumb (§18.1): a count in browse and naming, the slot's
/// chord in a Groupings edit, nothing otherwise. Pure so a test can read
/// it without laying out a window.
pub(crate) fn crumb_text(shell: &ShellView) -> String {
    let Some(state) = shell.object_dialog.as_ref() else { return String::new() };
    match &state.stage {
        Stage::Edit { object } if state.domain == Domain::Groupings => format!("ctrl+{object}"),
        Stage::Edit { .. } => String::new(),
        Stage::Browse | Stage::Naming => {
            let n = derive_rows(shell).len();
            format!("{n} {}", state.domain.crumb_noun())
        }
    }
}
```

`open` sets `dialog::set_title_extra(view, |shell, cx| { let state = shell.object_dialog.as_ref(); h_flex().gap_2().items_center().child(div().font_family(MONO).text_xs().text_color(cx.theme().muted_foreground).debug_selector(|| "objectdialog-crumb".into()).child(crumb_text(shell))).children(state.map(|s| dialog::mode_pill(s.mode, cx))).into_any_element() })`.

Browse rows: the layer text becomes `dialog::badge(layer.name(), muted_foreground, border, Some(format!("objectdialog-layer-{name}")), cx)`; `overridden` becomes `badge("overridden", theme.primary, theme.primary, Some(..), cx)`; `drifted` (always false today) would be a muted badge — paint it when true. Summary in `crate::fonts::MONO`. Edit header: same badges plus `badge("new", primary, primary, Some("objectdialog-new-badge"), cx)` when `draft.is_new && row.is_none()`. Field rows: right side gains `badge(match field.dest { Doc => "doc", Presentation => "pres" }, muted_foreground, border, Some(format!("objectdialog-dest-{}", field.key)), cx)` after the value. Item rows: `h_flex` of grip `div().text_color(muted_foreground).w(px(11.)).child("⋮")` (only for members of a reorderable list — every item in Groupings, members in Views; available items get an empty spacer of the same width), tick `div().font_family(MONO).w(px(13.)).text_color(if included { theme.success } else { muted_foreground }).child(if included { "✓" } else { "·" })`, then the name. Section header: for the first item whose `member` differs from the previous item's (or the first item of the list), wrap the row in `v_flex().child(header).child(row)` where `header` is `div().font_family(MONO).text_xs().text_color(muted_foreground).pt_2().pb_0p5().child(text)` with `.debug_selector` `objectdialog-section-members-{key}` / `objectdialog-section-available-{key}`; texts: Views members `"COLUMNS — space hides · shift+j / shift+k reorder · x removes"`, Views available `"AVAILABLE — space adds"`, Groupings `"DIMENSIONS — space includes · shift+j / shift+k reorder"` (uppercase in the *text* with letter spacing via `.tracking_wide()` if the pinned gpui offers it; otherwise uppercase alone). The wrapping keeps the list's child count equal to the visible row count, so `scroll_to_item` keeps indexing rows. Action bar: `Button::new(..).small().outline()` instead of `.ghost()`, `.danger()` kept. Footer: add `chip("x"), sep("remove ·")` to Views' edit hints.

- [ ] **Step 4: Run all shell tests**

Run: `cargo test -p geode-shell --features test-support`
Expected: PASS.

- [ ] **Step 5: Display check.** `cargo run -p geode-app -- --demo 1000`, open `Views: Config` (or whatever the palette names `config::views`), `config::groupings`, `config::scopes`; compare each stage against the artifact's tab. Note deviations in the Task 9 as-built. Fix obvious spacing here; do not chase pixel parity.

- [ ] **Step 6: Mutation entry**

```sh
# The section header rides on the first item's element so the list's
# child count still equals its row count; a header emitted as its own
# child makes scroll_to_item follow the wrong row from that point on.
run_mutation "objectdialog: section headers do not add list children" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '<the exact line that wraps the first item row with its header — from the code as written>' \
  '<the same line emitting the header as list.child(header) before the row>' \
  geode-shell \
  the_edit_stage_paints_section_headers_destination_badges_and_the_crumb
```

If no assertion can see a wrong `scroll_to_item` target, write the entry against the selector assertion above and note in its comment that it guards structure, not scrolling.

- [ ] **Step 7: Commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings
git add -A crates/geode-shell scripts/mutation-check.sh
git commit -m "feat(config): object dialog chrome to the mock — crumb, badges, grip and tick, section headers (§18.1)"
```

---

### Task 9: As-built, CLAUDE.md, harness run

**Files:**
- Modify: `docs/superpowers/specs/2026-09-08-geode-phase-4c-config-dialogs-design.md` (+ "### 18.6 As built")
- Modify: `CLAUDE.md` (a short paragraph after the Part 2a one)
- Modify: `scripts/mutation-check.sh` (the header's entry count, if it states one)

- [ ] **Step 1: Run the harness on the changed files, detached**

```bash
git status --porcelain   # must be empty
nohup zsh scripts/mutation-check.sh --changed=main > /tmp/mut-refinement.log 2>&1 &
```

Poll `/tmp/mut-refinement.log` until it ends. Every entry added by Tasks 1–8 must read `CAUGHT`; a `SURVIVED` is a missing test — fix the test (or the entry, if it mutates something other than what its name claims) before continuing. `git status --porcelain` must be empty afterwards.

- [ ] **Step 2: Write §18.6** — one bullet per deviation from §18.1–§18.5 as built, in §17's voice: what shipped, why it differs, what a maintainer must not tidy. Known candidates to check and record honestly: `x` is Views-only and inert elsewhere; a removed column re-enters the available block at its end, not in schema order; the `new` badge disappears as soon as the zero-debounce flush lands (a tick later), so it is visible for one frame in practice — say whether that is acceptable or whether `is_new` should persist for the stage's lifetime (recommended: persist — read `is_new` alone, not `editing_row().is_none()`); the naming stage paints the pill as `filter` because the `Input` owns the keys; the edit-stage filter clears on entering the stage from browse.

- [ ] **Step 3: CLAUDE.md** — after the Part 2a paragraph:

> **Phase 4c Part 2 refinement is done** (spec §18): the Groupings dialog always lists slots 1–9 (`Domain::roster`; `ObjectRow.layer` is `Option<Layer>`, `None` for an unfilled slot, and every destructive or forking decision reads it as such); `n` creates an object through a committed name (`Stage::Naming`, `check_object_name` in `geode-core`, `apply::commit_create` — one Doc write at zero debounce), with Views listing the dataset's other columns as non-members (`ListItem.member`; `space` adds, `x` removes, both Doc writes; hiding stays Presentation) and Scopes' `n` saving the frame's current scope; the edit stage filters (`Draft::query`, verbs resolve the filtered cursor by identity, a reorder moves past hidden rows); and the chrome matches the 2026-09-09 artifact — `ShellModal.title_extra` carries crumb and mode pill in the title row for every modal dialog, `dialog::badge` is the one classification pill, section headers ride on the first item's element so `scroll_to_item` still indexes rows.

- [ ] **Step 4: Anchors and CI checks**

```bash
zsh scripts/mutation-check.sh --anchors-only
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo bench --workspace --no-run && cargo check -p geode-shell --features test-support --all-targets
```

- [ ] **Step 5: Commit**

```bash
git add docs/superpowers/specs/2026-09-08-geode-phase-4c-config-dialogs-design.md CLAUDE.md scripts/mutation-check.sh
git commit -m "docs: Phase 4c §18.6 as built; CLAUDE.md refinement paragraph"
```

Then hand off to `superpowers:finishing-a-development-branch`.

---

## Self-review

- **Spec coverage:** §18.1 → Tasks 7, 8. §18.2 → Tasks 2, 3, 4, 5 (Views' two sections in 4; Scopes' frame read in 5; Groupings' inert `n` in 5; Schema's `writable()` is Part 2b and is not built here — §18.2 says "will"). §18.3 → Task 6. §18.4 → Task 1. §18.5 → each task's tests and entries, Task 9's run.
- **Placeholders:** Task 4's first entry and Task 8's entry ask the implementer to copy the anchor from the code as written, because the exact line is decided during implementation; both say what the mutation must change. Nothing else is deferred.
- **Type consistency:** `ObjectRow.layer: Option<Layer>` from Task 1 is what Tasks 5 and 8 read; `Draft::selected_row()` keeps its name through Task 6 (only its body changes), so Task 4's `remove_selected` written against it needs no rewrite; `move_item` changes from `bool` to `Option<usize>` in Task 6 — Task 4's test uses `!draft.move_item(1)` and must become `draft.move_item(1).is_none()` when Task 6 lands (Task 6's implementer: fix that call site).
