# Blotter Sort Identity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the blotter's sort and cursor follow the column a trader chose, by name, instead of the position it happened to occupy.

**Architecture:** `SortSpec.column` changes from a `usize` index into `ColumnPlan::columns` to the column's name, which is the identity `:sort` already resolves against. Every site that currently compares or stores an index compares or stores a name. `move_column` then needs no remap, because a name is stable under reorder, and a plan rebuild re-resolves the name instead of bounds-checking a stale index. `Cursor.col` keeps its index, because a cursor is a screen position, but it is re-derived by name across a column move so it follows the column the trader was looking at.

**Tech Stack:** Rust 2024, `geode-blotter` over `geode-core`, gpui-component's `DataTable` delegate, the project's zsh mutation harness.

**Spec:** `docs/superpowers/specs/2026-09-26-geode-silent-wrong-data-design.md`, branch one (§3).

## Global Constraints

- Comments state the local invariant and the failure they prevent. They must NOT cite a task number, phase number, spec section, review finding id, or date. This is a `CLAUDE.md` rule.
- Every new mutation entry names its covering test as the sixth argument to `run_mutation`, and a mutation must still COMPILE — one that fails to build reports `caught` for a build error rather than a test failure.
- `--anchors-only` now lints those filters and fails on one that matches no test, so a new entry naming nothing fails the gate.
- Gates: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `zsh scripts/mutation-check.sh --anchors-only`.
- Never run the harness with no argument; an unfiltered run takes about an hour. Use `--changed` while iterating.
- The tree column is never sortable and never named. Both sort entry points already guard it: the keyboard path with `if col == 0 { return; }` (`tile.rs:1083-1085`) and the click path by refusing `ColumnKind::Tree` (`delegate.rs:810-817`). `PlannedColumn::name` is empty for that column, so a name-keyed sort never has to represent it. Do not remove either guard.
- Resolution is first-match by name, exactly as `:sort` already does with `position(|c| c.name == column)`. A duplicate column name is a pre-existing configuration concern that branch three addresses; do not add a uniqueness check here.

---

## File Structure

| File | Change | Responsibility |
|---|---|---|
| `crates/geode-blotter/src/core/flatten.rs` | Modify `SortSpec` (`:95-99`) and `sort_siblings` (`:188-191`); tests at `:374`, `:430`, `:495` | Owns the sort's identity and applies it |
| `crates/geode-blotter/src/core/plan.rs` | Add a name lookup beside `attribution` (`:145`) | Resolves a name to a plan position |
| `crates/geode-blotter/src/delegate.rs` | Modify the sort field (`:66`), the click cycle (`:807-845`), `apply_snapshot`'s filter (`:398-404`), `move_column` (`:857-871`), and `column()`'s arrow | Holds the sort, re-resolves it, paints the arrow |
| `crates/geode-blotter/src/tile.rs` | Modify the keyboard sort (`:1080-1098`) and `:sort` (`:1176-1195`); read the notice slot `error` (`:194`) | The two entry points, and the notice when a sort is dropped |
| `scripts/mutation-check.sh` | Add three entries | Proves the three new contracts fail without their fix |

Two decisions this structure locks in. `SortSpec` keeps living in `flatten.rs` because that is what applies it. And the name-to-position lookup goes on `ColumnPlan`, not on the delegate, because the plan owns the column list and the pure tests need it without a window.

---

### Task 1: SortSpec carries a name

Make the type change and fix every site the compiler points at, without yet adding the notice or the cursor behaviour. This task ends with the existing suite green and the sort already immune to `move_column`.

**Files:**
- Modify: `crates/geode-blotter/src/core/flatten.rs:95-99` (`SortSpec`), `:188-191` (`sort_siblings`)
- Modify: `crates/geode-blotter/src/core/plan.rs` (new lookup)
- Modify: `crates/geode-blotter/src/delegate.rs:66`, `:807-845`, `:398-404`
- Modify: `crates/geode-blotter/src/tile.rs:1080-1098`, `:1176-1195`
- Test: the three existing `SortSpec` literals in `flatten.rs` tests (`:377`, `:386`, `:430`, `:495`)

**Interfaces:**
- Consumes: `PlannedColumn::name` (`plan.rs:26-28`), a `String` that is empty only for the tree column.
- Produces: `SortSpec { column: String, order: SortOrder }`, and `ColumnPlan::position_of(&self, name: &str) -> Option<usize>`. Later tasks use both.

- [ ] **Step 1: Write the failing test**

Add to `flatten.rs`'s test module. It fails to compile first, which is the point: it names the field as a string.

```rust
    #[test]
    fn a_sort_survives_a_column_move_because_it_names_the_column() {
        let snapshot = fixture();
        let mut plan = plan_for(&snapshot);
        let delta = plan
            .columns
            .iter()
            .position(|c| c.name == "delta01")
            .expect("the fixture has delta01");
        let spec = SortSpec {
            column: "delta01".to_string(),
            order: SortOrder::Desc,
        };
        let before = visible_with(&snapshot, &plan, Some(&spec));

        // Moving a column must not change which column the sort names.
        plan.move_column(delta, delta + 1);
        let after = visible_with(&snapshot, &plan, Some(&spec));

        assert_eq!(
            before, after,
            "the sort follows delta01, not the position it used to hold"
        );
    }
```

Use the fixture and helper names the neighbouring tests already use. `flatten.rs`'s test module has a `visible(expansion, sort)` helper at `:345`; if it does not take a plan, add a sibling that does rather than changing its signature, and say so in your report.

- [ ] **Step 2: Run it and watch it fail to compile**

Run: `cargo test -p geode-blotter --lib -- a_sort_survives_a_column_move`

Expected: a compile error that `column` expects `usize`, found `String`. That is the correct first failure; the type has not changed yet.

- [ ] **Step 3: Change the type**

In `crates/geode-blotter/src/core/flatten.rs`:

```rust
pub struct SortSpec {
    /// The snapshot column name, as `PlannedColumn::name` spells it. A name
    /// rather than a position because the plan reorders under a column drag
    /// and shortens when a column is hidden or folded into the tree, and an
    /// index that survives either change names a different column.
    pub column: String,
    pub order: SortOrder,
}
```

`SortSpec` loses `Copy`. Every site that copied it now clones or borrows; the compiler will name them all.

- [ ] **Step 4: Add the lookup to ColumnPlan**

In `crates/geode-blotter/src/core/plan.rs`, beside `attribution`:

```rust
    /// The position a named column currently occupies, or `None` when the
    /// plan no longer has it. First match, as `:sort` resolves a name.
    pub fn position_of(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c.name == name)
    }
```

- [ ] **Step 5: Resolve the name where the sort is applied**

In `sort_siblings`, replace the index lookup:

```rust
fn sort_siblings(snapshot: &Snapshot, plan: &ColumnPlan, spec: &SortSpec, rows: &mut [u32]) {
    let Some(column) = plan.position_of(&spec.column).and_then(|i| plan.columns.get(i)) else {
        return;
    };
```

The early return already handles a column the plan does not have, so an unresolvable sort is inert rather than a panic. Leave that behaviour.

- [ ] **Step 6: Fix the four call sites the compiler names**

`delegate.rs:835` (click cycle) and `tile.rs:1093` (keyboard) both build a `SortSpec` from a column index and compare with `s.column == col`. Resolve through the plan instead. For the click cycle:

```rust
        let name = match self.plan.as_ref().and_then(|p| p.columns.get(col_ix)) {
            Some(c) => c.name.clone(),
            None => return,
        };
        let current = self
            .sort
            .as_ref()
            .filter(|s| s.column == name)
            .map(|s| s.order);
        let next = SortOrder::click_cycle(current, self.is_measure(col_ix));
        self.sort = next.map(|order| SortSpec {
            column: name,
            order,
        });
```

The keyboard path at `tile.rs:1088-1093` takes the same shape against `d.cursor.col`. `tile.rs:1187` (`:sort`) already has the name in hand from the command, so it stores `column: column.clone()` and no longer needs `position()` except to compute `is_measure`.

- [ ] **Step 7: Make the plan rebuild re-resolve rather than bounds-check**

In `delegate.rs`, replace the filter at `:398-404`:

```rust
        if rebuild {
            self.plan = Some(fresh);
            // Re-resolved, not bounds-checked: a rebuild that hides a column
            // or folds a dimension into the tree shortens the list, and an
            // in-range index then names a different column than the trader
            // sorted by.
            self.sort = self
                .sort
                .take()
                .filter(|s| self.plan.as_ref().is_some_and(|p| p.position_of(&s.column).is_some()));
        }
```

Task 3 adds the notice on the dropped case; leave it silent for now.

- [ ] **Step 8: Delete the remap that move_column never had**

`delegate.rs`'s `move_column` hook (`:857-871`) needs no change at all: it reorders the plan and the sort names a column, so nothing can go stale. Add one comment saying so, because its absence is now load-bearing:

```rust
        // No sort or cursor remap here: the sort names its column and the
        // cursor is re-derived below, so a reorder cannot re-point either.
```

Only write that comment once Task 2 has made the cursor half true. If you reach this step first, write only the sort half.

- [ ] **Step 9: Update the three existing test literals**

`flatten.rs:377`, `:386`, `:430` and `:495` build `SortSpec` with integer columns. Change each to the name of the column that index referred to in that fixture. Read the fixture to find it; do not guess.

- [ ] **Step 10: Run the crate suite**

Run: `cargo test -p geode-blotter --lib`

Expected: all pass, including the new test from Step 1.

- [ ] **Step 11: Commit**

```bash
git add crates/geode-blotter/src scripts/mutation-check.sh
git commit -m "fix(blotter): a sort names its column, not a position

SortSpec.column was an index into ColumnPlan::columns, and two paths
shifted it. A column drag reorders that vector and never remapped the
sort, so the recorded sort named a position now holding a different
column and the header painted its arrow there. A plan rebuild only
bounds-checked the index, so hiding a column or folding a dimension into
the tree shortened the list and an in-range sort reordered the rows by
something the trader never chose.

The sort now names the column, which is the identity :sort already
resolves, so a reorder cannot re-point it and a rebuild re-resolves it."
```

---

### Task 2: the cursor follows its column across a move

**Files:**
- Modify: `crates/geode-blotter/src/delegate.rs` (`move_column` hook, `:857-871`)
- Test: `crates/geode-blotter/src/delegate.rs` test module

**Interfaces:**
- Consumes: `ColumnPlan::position_of` from Task 1.
- Produces: nothing later tasks rely on.

`Cursor.col` (`core/cursor.rs:11-14`) stays a `usize`, because a cursor is where the highlight sits on screen. What is wrong today is that a column move leaves it on a position, so the column under the highlight changes and a subsequent `s` cycles the sort of a different column.

- [ ] **Step 1: Write the failing test**

```rust
    #[gpui::test]
    fn the_cursor_follows_its_column_across_a_move(cx: &mut gpui::TestAppContext) {
        // Build the delegate the way the neighbouring delegate tests do.
        let mut d = delegate_fixture(cx);
        let from = d.plan.as_ref().unwrap().position_of("delta01").unwrap();
        d.cursor.col = from;
        let name_before = d.plan.as_ref().unwrap().columns[d.cursor.col].name.clone();

        d.move_column_for_test(from, from + 1);

        let name_after = d.plan.as_ref().unwrap().columns[d.cursor.col].name.clone();
        assert_eq!(
            name_before, name_after,
            "the cursor rests on the column it rested on, not the position"
        );
    }
```

Use the real fixture and the real route the neighbouring tests use to reach `move_column`. If they drive it through the `TableDelegate` hook rather than a helper, do that instead of adding `move_column_for_test`, and say which in your report.

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p geode-blotter --lib -- the_cursor_follows_its_column_across_a_move`

Expected: FAIL, with `name_after` naming the column that moved into the old position.

- [ ] **Step 3: Re-derive the cursor in the move hook**

In the `move_column` hook, capture the name before the reorder and restore the position after:

```rust
        // Captured before the reorder: the cursor is a screen position, so a
        // reorder would otherwise leave it on a different column and the next
        // sort would cycle something the trader was not looking at.
        let under_cursor = self
            .plan
            .as_ref()
            .and_then(|p| p.columns.get(self.cursor.col))
            .map(|c| c.name.clone());
        if let Some(p) = self.plan.as_mut() {
            p.move_column(col_ix, to_ix);
        }
        if let Some(name) = under_cursor
            && let Some(i) = self.plan.as_ref().and_then(|p| p.position_of(&name))
        {
            self.cursor.col = i;
        }
```

- [ ] **Step 4: Run the test, then the crate suite**

Run: `cargo test -p geode-blotter --lib -- the_cursor_follows_its_column_across_a_move`
Expected: PASS.

Run: `cargo test -p geode-blotter --lib`
Expected: all pass.

- [ ] **Step 5: Complete Task 1 Step 8's comment**

Now that both halves are true, the comment in the move hook can say so. Make sure it reads as one statement about both, not two appended sentences.

- [ ] **Step 6: Commit**

```bash
git add crates/geode-blotter/src
git commit -m "fix(blotter): the cursor follows its column across a move

Cursor.col is a screen position, which is right, but a column drag left
it pointing at whatever slid into that slot. The next s then cycled the
sort of a column the trader was not looking at. The move hook now
captures the name under the cursor and restores the position after the
reorder."
```

---

### Task 3: a dropped sort says so

**Files:**
- Modify: `crates/geode-blotter/src/delegate.rs` (`apply_snapshot`, the filter from Task 1 Step 7)
- Modify: `crates/geode-blotter/src/tile.rs` (read the dropped name into `error`, `:194`)
- Test: `crates/geode-blotter/src/tile.rs` test module

**Interfaces:**
- Consumes: the re-resolving filter from Task 1.
- Produces: nothing later tasks rely on.

Dropping the sort reorders the rows back to default order, so the trader sees movement either way. The notice is the difference between a surprise and an explanation.

- [ ] **Step 1: Write the failing test**

```rust
    #[gpui::test]
    fn hiding_the_sorted_column_drops_the_sort_and_says_which(cx: &mut gpui::TestAppContext) {
        let mut tile = blotter_fixture(cx);
        sort_by(&mut tile, "delta01", cx);

        // A view edit that hides delta01 shortens the plan.
        apply_view_without(&mut tile, "delta01", cx);

        assert!(
            sort_of(&tile, cx).is_none(),
            "the sorted column is gone, so the sort is gone"
        );
        let notice = notice_of(&tile, cx).expect("a notice names the dropped sort");
        assert!(
            notice.contains("delta01"),
            "the notice names the column whose sort went: {notice}"
        );
    }
```

Use the real fixture and the real route to apply a view without a column, as the neighbouring tile tests do. Name in your report which helpers you used in place of `sort_by`, `apply_view_without`, `sort_of` and `notice_of`.

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p geode-blotter --lib -- hiding_the_sorted_column_drops_the_sort_and_says_which`

Expected: FAIL on the notice assertion. The sort is already dropped by Task 1; nothing reports it.

- [ ] **Step 3: Report the dropped name**

Have the re-resolving filter surface what it dropped, rather than discarding it. In `apply_snapshot`:

```rust
        let mut dropped_sort = None;
        if rebuild {
            self.plan = Some(fresh);
            if let Some(s) = self.sort.take() {
                match self.plan.as_ref().and_then(|p| p.position_of(&s.column)) {
                    Some(_) => self.sort = Some(s),
                    // The rows reorder to default either way, so the trader is
                    // told which column's sort went rather than left to infer it.
                    None => dropped_sort = Some(s.column),
                }
            }
        }
```

Return it to the caller alongside whatever `apply_snapshot` already returns, and have the tile set `self.error = Some(format!("sort on '{name}' dropped: the column is no longer in this view"))`.

If `apply_snapshot`'s signature cannot carry it cleanly, put the dropped name in a field on the delegate that the tile reads and clears, and say which you chose and why in your report.

- [ ] **Step 4: Run the test, then the crate suite**

Run: `cargo test -p geode-blotter --lib -- hiding_the_sorted_column_drops_the_sort_and_says_which`
Expected: PASS.

Run: `cargo test -p geode-blotter --lib`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-blotter/src
git commit -m "fix(blotter): a dropped sort names the column it was on

Hiding the sorted column or folding it into the tree drops the sort, and
the rows reorder to default order whichever way that is reported. The
tile now names the column in its notice, so the reorder reads as an
explanation rather than a surprise."
```

---

### Task 4: mutation entries for the three contracts

Three contracts changed, and all three are silent-wrong-data contracts, which is exactly what the harness is for.

**Files:**
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Consumes: the three tests from Tasks 1 to 3.
- Produces: nothing.

- [ ] **Step 1: Add the three entries**

Append beside the other `geode-blotter` sort entries, which sit around `scripts/mutation-check.sh:3009-3044`.

```zsh
# A sort that names a position is re-pointed by any reorder: the rows then
# order by a column the trader never chose and the header agrees with it.
run_mutation "blotter sort: a reorder cannot re-point a sort" \
  crates/geode-blotter/src/core/flatten.rs \
  '    let Some(column) = plan.position_of(&spec.column).and_then(|i| plan.columns.get(i)) else {' \
  '    let Some(column) = plan.columns.get(0) else {' \
  geode-blotter \
  a_sort_survives_a_column_move_because_it_names_the_column

# A rebuild that only bounds-checks leaves an in-range index naming a
# different column, and the siblings reorder by it in the same frame.
run_mutation "blotter sort: a rebuild re-resolves rather than bounds-checks" \
  crates/geode-blotter/src/delegate.rs \
  '                match self.plan.as_ref().and_then(|p| p.position_of(&s.column)) {' \
  '                match Some(0usize) {' \
  geode-blotter \
  hiding_the_sorted_column_drops_the_sort_and_says_which

# The cursor is a screen position, so a reorder leaves it on a different
# column and the next sort cycles something else.
run_mutation "blotter cursor: a reorder cannot re-point the cursor" \
  crates/geode-blotter/src/delegate.rs \
  '        if let Some(name) = under_cursor' \
  '        if let Some(name) = None::<String>' \
  geode-blotter \
  the_cursor_follows_its_column_across_a_move
```

Anchor text must match your implementation exactly. If your code differs from the plan's, anchor what you wrote, not what the plan predicted.

- [ ] **Step 2: Probe all three**

```bash
zsh scripts/mutation-check.sh "blotter sort: a reorder cannot re-point a sort"
zsh scripts/mutation-check.sh "blotter sort: a rebuild re-resolves"
zsh scripts/mutation-check.sh "blotter cursor: a reorder cannot re-point the cursor"
```

Expected: `caught` for each.

A `SURVIVED` means the test does not actually see that break: report it rather than re-aiming at a test that happens to pass. A suspiciously fast `caught` means the mutated crate did not compile, which is `caught` for a build error rather than a test failure; check the build.

- [ ] **Step 3: Confirm the gate**

Run: `zsh scripts/mutation-check.sh --anchors-only; echo "exit $?"`

Expected: exit 0, three more anchors than before, `0 stale, 0 ambiguous, 0 bad filters`.

- [ ] **Step 4: Run every gate CI runs**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
zsh scripts/mutation-check.sh --anchors-only
```

Expected: all clean.

- [ ] **Step 5: Update the crate README's invariants**

`crates/geode-blotter/README.md` states the blotter's narrow invariants. Add that a sort and the cursor are held by column name and re-resolved on a plan rebuild, and that a sort whose column leaves the view is dropped with a notice. Match the file's existing voice; no task or review numbers.

- [ ] **Step 6: Commit**

```bash
git add scripts/mutation-check.sh crates/geode-blotter/README.md
git commit -m "test(blotter): mutation entries for sort and cursor identity

Three contracts changed and all three fail silently when broken: a
reorder re-pointing a sort, a rebuild bounds-checking instead of
re-resolving, and a reorder re-pointing the cursor. Each entry breaks one
and names the test that sees it."
```

---

## Self-Review

**Spec coverage.** §3.2 of the spec has four bullets. `SortSpec` becomes a name (Task 1). `move_column` needs no remap, stated as a comment because its absence is load-bearing (Task 1 Step 8, completed in Task 2 Step 5). A rebuild re-resolves (Task 1 Step 7). The notice names the dropped column (Task 3). `Cursor.col` follows the same identity (Task 2). §3.3's failure semantics are covered: an unresolvable sort is inert via `sort_siblings`' early return, and a dropped sort is reported. §7's verification duties are Task 4.

**Placeholders.** Three tasks name fixture helpers as stand-ins (`visible_with`, `delegate_fixture`, `move_column_for_test`, `blotter_fixture`, `sort_by`, `apply_view_without`, `sort_of`, `notice_of`) and each instructs the implementer to read the neighbouring tests for the real names and report which it used. That is deliberate: I read the production code closely but not each test module's helper set, and inventing helper names would be worse than saying so. Every production code block is literal.

**Type consistency.** `SortSpec.column` is `String` from Task 1 onward, and Tasks 2 to 4 use it as such. `ColumnPlan::position_of(&self, name: &str) -> Option<usize>` is defined in Task 1 Step 4 and consumed in Task 1 Step 5, Task 2 Step 3, and Task 3 Step 3. `Cursor.col` stays `usize` throughout, which Task 2 states explicitly so nobody changes it. `SortSpec` loses `Copy` in Task 1 Step 3, which is why later sites use `as_ref()` and `take()`.

**One risk the implementer should know.** `SortSpec` losing `Copy` will produce a cluster of borrow errors the plan cannot enumerate, because they depend on which sites currently rely on the copy. That is expected and mechanical; if it turns out to be more than a handful, report it rather than restructuring the surrounding code.
