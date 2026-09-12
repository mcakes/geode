# Membership Is a List — Two Lists in `OrderedList`

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the `member` flag and its hand-maintained ordering invariant with two lists, `items` and `available`, and a third `EditRow` variant, so that membership is structural and every consumer of an available row is forced to say what it means.

**Architecture:** `FieldKind::OrderedList { items, available }`; `ListItem` loses `member`; `EditRow::Available { field, item }` joins `Field` and `Item`. The four mutators move entries between the two vectors instead of flipping a flag; the three write-side functions and `membership_changed` read `items` with no filter; the render paints one header per non-empty list. Behaviour is unchanged.

**Tech Stack:** Rust, gpui + gpui-component (pinned), `scripts/mutation-check.sh`.

**Spec:** `docs/superpowers/specs/2026-09-08-geode-phase-4c-config-dialogs-design.md` — **§18.7 is this plan's brief**; §18.2, §18.6 (the 2026-09-11 cursor rulings for `space`/`x`, the Key/Attribute/derived exclusions, `kind`) are the background it must preserve.

## Global Constraints

- CI on macOS and Windows: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo bench --workspace --no-run`, `cargo check -p geode-shell --features test-support --all-targets`.
- **Behaviour is unchanged.** Every existing test in `crates/geode-shell/src/shell/tests/objectdialog.rs` (150) passes UNMODIFIED. A window test that needs changing is a sign the refactor changed behaviour — stop and report BLOCKED. Pure tests in `mod.rs`/`views.rs`/`groupings.rs` that assert on the flag are rewritten to the two-list shape without weakening what they pin.
- **TDD** for the new model: the pure tests come first and fail to compile; the existing window suite is the regression net.
- **A mutation entry for every behaviour kept**: the five entries anchored on flag lines (`objectdialog: membership compares member names only`, `…reorder never crosses the member boundary`, `…the doc table lists members only`, `…x refuses an available row rather than reordering it`, `…refresh_available empties the old dataset's rows`) are each re-anchored onto the structural line that now carries the behaviour, still caught by their named test — or retired with a comment when the behaviour is unrepresentable (the boundary crossing: there is no boundary to cross). `--anchors-only` exits 0 before every commit; confirm each re-anchored entry `caught` by running it.
- A doc comment that contradicts the code is a defect: `ListItem`'s and `FieldKind`'s docs, `views.rs`'s module doc, `groupings.rs`'s "every item here IS a member" comment, `render.rs`'s section-header comments, `Draft::step_selected`'s and `remove_selected`'s docs all describe the flag — rewrite each.
- Never a raw colour. Nothing stalls the render thread. `geode-shell` never depends on `geode-data`.
- Do not change the cursor rules of §18.6 (`space` leaves the cursor on the next available row; `x` on the row that was next, stepping back on the last row).

## As-built vocabulary this plan builds on

```rust
// crates/geode-shell/src/shell/objectdialog/mod.rs (main at 4d74399)
pub struct ListItem { pub name: String, pub included: bool, pub width: Option<f32>, pub member: bool, pub kind: Option<String> }
pub enum FieldKind { Text(String), Number{..}, Bool(bool), Choice{..}, MultiChoice{..}, OrderedList { items: Vec<ListItem> } }
pub enum EditRow { Field(usize), Item { field: usize, item: usize } }
impl Draft {
  pub fn rows(&self) -> Vec<EditRow>                 // Field, then every item
  pub fn row_label(&self, row: EditRow) -> String
  pub fn visible_rows(&self) -> Vec<Ranked>          // row-ordered
  pub fn selected_row(&self) -> Option<EditRow>
  fn follow(&mut self, row: EditRow)
  pub fn list_items(&self, key: &str) -> Option<&[ListItem]>
  fn step_selected(&mut self, dir) -> Step           // Item arm: `if !items[item].member { add … }` else toggle included
  pub fn move_item(&mut self, delta: i32) -> Option<usize>   // refuses crossing `items[next].member != block`
  pub fn remove_selected(&mut self) -> Step          // Refused on dest==Doc; Refused on !member; else demote to end
}
fn membership_changed(before, field) -> bool        // names of items.filter(member)
// views.rs: fields() builds members then non-members; refresh_available(draft, config) retains members and rebuilds the rest;
//   doc_table/doc_baseline/presentation_table filter `i.member`; schema_role_kind(role) -> Option<&str>
// groupings.rs: fields() builds every item with member: true (ticked chain first, then unticked pickables)
// render.rs build_edit: section header when `(field, entry.member)` changes; grip iff entry.member; field_value counts member items
// tests: mod.rs `list_items(draft)` helper, views.rs `tree_with_two_available_columns`, many `i.member` assertions
```

---

### Task 1: The two-list model, end to end

**Files:**
- Modify: `crates/geode-shell/src/shell/objectdialog/mod.rs`
- Modify: `crates/geode-shell/src/shell/objectdialog/views.rs`
- Modify: `crates/geode-shell/src/shell/objectdialog/groupings.rs`
- Modify: `crates/geode-shell/src/shell/objectdialog/render.rs`
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Produces: `FieldKind::OrderedList { items: Vec<ListItem>, available: Vec<ListItem> }`; `ListItem` without `member`; `EditRow::Available { field: usize, item: usize }`; `Draft::available_items(&self, key: &str) -> Option<&[ListItem]>` beside `list_items`.

- [ ] **Step 1: Failing pure tests.** Rewrite the existing flag tests in `mod.rs`'s and `views.rs`'s test modules to the new shape, and add these (they fail to compile until Step 2):

```rust
// views.rs tests — replaces the `(name, member, included, kind)` shape assertion
#[test]
fn the_column_list_is_members_then_the_datasets_other_columns() {
    let draft = Domain::Views.draft(&tree_with_two_available_columns(), "tree");
    let items: Vec<&str> = draft.list_items("columns").unwrap().iter().map(|i| i.name.as_str()).collect();
    let available: Vec<&str> = draft.available_items("columns").unwrap().iter().map(|i| i.name.as_str()).collect();
    assert_eq!(items, ["npv"]);
    assert_eq!(available, ["book", "delta01"]);
    assert!(draft.rows().contains(&EditRow::Available { field: 1, item: 0 }));
}

#[test]
fn adding_moves_an_available_row_into_the_members_and_is_a_doc_write() {
    let mut draft = Domain::Views.draft(&tree_with_two_available_columns(), "tree");
    draft.selected = 3; // Field(dataset)=0, Field(columns)=1, Item npv=2, Available book=3
    assert_eq!(draft.selected_row(), Some(EditRow::Available { field: 1, item: 0 }));
    assert!(draft.toggle_selected().changed());
    let items: Vec<&str> = draft.list_items("columns").unwrap().iter().map(|i| i.name.as_str()).collect();
    assert_eq!(items, ["npv", "book"]);
    assert_eq!(draft.available_items("columns").unwrap().len(), 1);
    assert!(draft.writes_by_destination().contains_key(&Destination::Doc));
    let text = super::super::object_text("tree", Domain::Views.to_table(&draft, Destination::Doc));
    assert!(text.contains("name = \"book\"") && text.contains("kind = \"dimension\""), "{text}");
    assert!(!text.contains("delta01"), "an available column never reaches views.toml: {text}");
}

#[test]
fn x_moves_a_member_to_the_end_of_available() {
    let mut draft = Domain::Views.draft(&tree_with_two_available_columns(), "tree");
    draft.selected = 2; // npv
    assert!(draft.remove_selected().changed());
    assert!(draft.list_items("columns").unwrap().is_empty());
    let available: Vec<&str> = draft.available_items("columns").unwrap().iter().map(|i| i.name.as_str()).collect();
    assert_eq!(available, ["book", "delta01", "npv"]);
}

#[test]
fn an_available_row_cannot_be_reordered() {
    let mut draft = Domain::Views.draft(&tree_with_two_available_columns(), "tree");
    draft.selected = 3; // book, available
    assert_eq!(draft.move_item(1), None);
    let available: Vec<&str> = draft.available_items("columns").unwrap().iter().map(|i| i.name.as_str()).collect();
    assert_eq!(available, ["book", "delta01"], "untouched");
}

// mod.rs tests, Groupings (no available block)
#[test]
fn a_groupings_list_has_no_available_block_and_x_refuses() {
    let config = config_from(&[(Layer::Builtin, "datasets", "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n"),
        (Layer::User, "groupings", "3 = [\"book\"]\n")]);
    let mut draft = Domain::Groupings.draft(&config, "3");
    assert!(draft.available_items("dimensions").unwrap().is_empty());
    draft.selected = 2;
    assert!(matches!(draft.remove_selected(), Step::Refused(_)));
}
```

Keep the existing cursor tests (`space` leaves the cursor on the next available row; `x` on the row that was next; last-row step-back) exactly as they are — they are the §18.6 rulings and must still pass.

- [ ] **Step 2: Implement the model** in `mod.rs`:

```rust
pub struct ListItem { pub name: String, pub included: bool, pub width: Option<f32>, pub kind: Option<String> }

pub enum FieldKind {
    …,
    /// The object's own ordered list, and — for a list a trader can add
    /// to — the catalogue of what may join it (spec §18.7). `items` is
    /// the only list that is ordered, written, counted or reorderable;
    /// `available` is unordered by construction and empty where ticking
    /// IS membership (Groupings). Two vectors rather than a flag, so the
    /// member-before-available rule is not a rule at all.
    OrderedList { items: Vec<ListItem>, available: Vec<ListItem> },
}

pub enum EditRow {
    Field(usize),
    Item { field: usize, item: usize },
    /// One row of a list's `available` block. A variant of its own so a
    /// consumer cannot treat an available row as a member by omission —
    /// every match on `EditRow` has to say what `space`, `x`, a click or
    /// a label means here.
    Available { field: usize, item: usize },
}
```

`rows()`: `Field(i)`, then `Item` for each of `items`, then `Available` for each of `available`. `row_label`: the item's name for both. `list_items` unchanged; add `available_items`. `step_selected`'s `EditRow::Available` arm: remove from `available`, set `included = true`, push onto `items`, then apply the §18.6 cursor rule verbatim (`self.selected = (self.selected + 1).min(last)` against `visible_rows()`); the `Item` arm keeps the toggle and the Doc-list refusal. `remove_selected`: `Item` in a list whose `available` is empty → `Refused("space unticks here")` (Groupings — decide by the block's emptiness, not `dest`, and say so in the doc: §18.7.2); `Available` → `Refused("not in the view — space adds it")`; `Item` otherwise → remove from `items`, `included = false`, push onto `available`, then the §18.6 cursor rule verbatim. `move_item`: `Available` → `None`; `Item` → the visibility walk within `items` only, no block check. `follow` handles the third variant. `membership_changed`: `items` names, no filter. `selected_field_is_steppable` in `render.rs`: `Available` is steppable.

- [ ] **Step 3: Adapters.** `views::fields` builds `items` from the view and `available` from `push_dataset_columns` (no `member:`); `refresh_available` assigns `available` wholesale; `doc_table`/`doc_baseline`/`presentation_table` take `items` with no filter; `columns_for` unchanged. `groupings::fields` builds `items` only, `available: Vec::new()`. `scopes.rs` has no list.

- [ ] **Step 4: Render.** In `build_edit`'s loop, the section header attaches to the first VISIBLE `Item` of a list (text: `COLUMNS`/`DIMENSIONS` per domain) and to the first VISIBLE `Available` of a list (`AVAILABLE — space adds`); the grip paints on `Item` only; `field_value` counts `items.len()` and `items.iter().filter(|i| !i.included).count()`. Selectors unchanged (`objectdialog-section-members-{key}`, `-available-{key}`, `objectdialog-item-{name}` for both kinds — the existing window tests click and assert on those).

- [ ] **Step 5: Run everything.** `cargo test -p geode-shell --features test-support` — 150 window tests unmodified and green; the rewritten pure tests green. Paste RED (the compile failure from Step 1) and GREEN.

- [ ] **Step 6: Harness.** Re-anchor: `membership compares member names only` → the `items` names line in `membership_changed` (mutate to compare `available` too, or to always `false`); `the doc table lists members only` → `doc_table`'s `columns_for(&draft.source, &items)` line (mutate to pass `items` chained with `available`); `x refuses an available row` → the `EditRow::Available => Refused` arm; `refresh_available empties the old dataset's rows` → the wholesale assignment; `reorder never crosses the member boundary` → RETIRE with a comment (no boundary exists) and add `objectdialog: an available row is not reorderable` on the `EditRow::Available => return None` arm, named test `an_available_row_cannot_be_reordered`. Confirm each `caught`; `--anchors-only` exit 0.

- [ ] **Step 7: Comments.** Every doc named in Global Constraints rewritten; `grep -rn 'member' crates/geode-shell/src/shell/objectdialog/` returns only prose that is true (the word survives in "members" as English; the field does not).

- [ ] **Step 8: Checks and commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test -p geode-shell --features test-support && zsh scripts/mutation-check.sh --anchors-only
git add -A crates/geode-shell scripts/mutation-check.sh
git commit -m "refactor(config): membership is a list — OrderedList { items, available }, EditRow::Available (§18.7)"
```

---

### Task 2: As-built, CLAUDE.md, harness run

- [ ] **Step 1:** `zsh scripts/mutation-check.sh --changed=main` detached; every entry `caught`; tree clean.
- [ ] **Step 2:** Spec §18.7.5 "As built": what shipped, the re-anchored and retired entries, any deviation.
- [ ] **Step 3:** CLAUDE.md: in the Part 2 refinement paragraph, replace the `ListItem.member` sentence with the two-list model and the `EditRow::Available` rule (a consumer of an available row must say what it means).
- [ ] **Step 4:** The five CI checks; commit `docs: §18.7 as built; CLAUDE.md two-list model`.

---

## Self-review

- **Spec coverage:** §18.7.1 → T1 steps 2–3; §18.7.2 → T1 step 2 (verbs) and the preserved cursor tests; §18.7.3 → T1 steps 3–4; §18.7.4 → T1 steps 1, 5, 6 and T2.
- **Placeholders:** none; the mutation targets are named per entry.
- **Type consistency:** `available_items(&self, key) -> Option<&[ListItem]>` mirrors `list_items`; `EditRow::Available { field, item }` mirrors `Item`.
