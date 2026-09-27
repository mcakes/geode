# Grid selection Part 3 — line pricer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `V` (rows) and `v` (cell block) selections to the line pricer, with these verbs over a selection:
- `y`, `p`/`P`, `d`, `J`/`K`, `g p` and `g u`;
- a one-value commit to every target line;
- a live relative arrow step that one `escape` rolls back;
- risk totals in the footer;
- shift+click and drag.

Every bulk change is one undo entry.

**Architecture:**
- The shared pure core `geode_core::grid::selection` supplies `Selection`, `resolve_with`, `Resolved`, `Lost` and `top_most`. It is merged, from Part 1.
- The pricer's selection rules go in a new pure module `crates/geode-pricer/src/core/select.rs`, over `Sheet`, with no gpui:
  - which lines an edit reaches (a package stands for its legs);
  - top-most rows;
  - the group and move plans;
  - skip counting and notices;
  - position risk totals.
- Tile-side state and verbs go in a new child module, `crates/geode-pricer/src/tile/select.rs`. It is an `impl PricerTile` block, and a child module can see the tile's private fields. That keeps `tile.rs` (9.4k lines) to hooks only.
- Part 2's market-data implementation is the reference for shape: `crates/geode-marketdata/src/tile/select.rs`, its `delegate.rs` pointer wiring, and its tests. Port its patterns, with the pricer's own vocabulary.

**Tech Stack:** Rust, GPUI plus gpui-component 0.6.2 `DataTable`, `geode_core::grid::selection`, and `geode_shell::shell::aggregates::strip`.

**Spec:** `docs/superpowers/specs/2026-09-26-grid-selection-design.md`. The relevant sections are:
- §4.1 lifecycle;
- §4.2 common verbs;
- §4.5 pricer, including the 2026-09-27 rulings on cell edits over a selection and the footer;
- §4.6 kind mismatch;
- §5 mouse;
- §6 keymaps.

## Global Constraints

- **Selection keys:** `V` gives `SelectKind::Rows` and `v` gives `SelectKind::Block`. The other key switches kind and keeps the anchor; the same key again clears the selection.
- **Escape:** `escape` clears only the selection. A second `escape` does today's `escape`.
- **Motion:** while a selection is live, motions clamp at the edges. `step_rows` already clamps. The cursor never enters the tree column.
- **Consuming vs repeatable verbs:**
  - `y`, `d`, `g p` and `g u` end the selection.
  - `J`/`K`, the typed commit and the step keep it.
  - A count on `J`/`K` and `g p` is ignored while a selection is live.
- **Anchor identity:** the row is the grid row's `LineId` and the column is the plan column's `def.name`.
  - If the anchor is lost (a collapse hides it, a delete removes it, a view hides its column), the selection clears with `selection cleared: anchor row no longer shown` or `selection cleared: anchor column no longer shown`.
  - Replacing the sheet (a load, `:new`, `:name`) clears the selection silently.
- **Tree column:** it is never a selection column. `Resolved.cols` are plan-column indices. The tree cell and its gutter are row handles for the mouse.
- **Doubled verbs are normal-mode only:** in visual mode the verbs are single keys (`y`, `d`, `shift+j`, `shift+k`, `g p`, `g u`, `i`, `enter`).
- **Kind mismatch:** row verbs refuse in a `Block` with these exact texts:
  - `d deletes rows — use V`
  - `shift+j/k move rows — use V`
  - `g p groups rows — use V`
  - `g u ungroups rows — use V`
- **Edits act on lines.** The target set is the deduplicated leaf lines of the selected rows: a line is itself, and a package is its legs, whether the package is open or not.
- **Columns:** under `V` the targets are the cursor's plan column only; under `v` they are the block's plan columns.
- **One undo entry:** every bulk change is one undo entry, applied through the tile's batch door. A refused batch leaves the sheet unchanged.
- **Notices:**
  - `set N cells[, skipped M (…)]`
  - `stepped N cells ±S[, skipped M (…)]`
  - `no selected cell accepts '…'[, skipped …]`
  - `deleted N rows`
- **Footer:** while a selection is live the footer shows the extent `R rows × C cols`, then position totals for `price delta gamma vega theta rho`, over the TOP-MOST selected rows. A line's total is `qty × value`; a package contributes its own folded sum, which is already qty-weighted. If any top-most row has no result or has failed, that column's total is `—`: never a partial sum. A footer refusal notice takes the footer while it stands.
- **Render discipline:** `Resolved`, the extent text and the totals are prepared at change points. Render only looks them up.
- **Tests:** every new test goes through a production route: `h.dispatch` (the `TileContent` door), `h.command`, or real mouse events at painted bounds.
- **Gates after every task:** `cargo fmt --check`, `cargo clippy -p geode-pricer --all-targets -- -D warnings` and `cargo test -p geode-pricer` pass.
- **Comments:** comments state the local invariant and the failure it prevents. No task numbers, no spec-section citations.

### Rulings made in this plan (surface to Matthew at handoff)

1. **Footer totals are position totals.** They are `qty × value` per line, and a package's own sum, which is already qty-weighted. The price column paints a line's UNIT price, so a line with `qty ≠ 1` totals differently from the cell above it. Summing the unit values would add a −5-lot spread's price to a 1-lot line's as if they were the same size.
2. **An incomplete total is `—`.** Any top-most row without a result, or one that failed, turns its column's total into `—`. A stale value still counts, since it is what the grid shows.
3. **`y` in `V` copies the top-most rows' shorthand,** one per line. Copying a package's legs as well would double them on `p`.
4. **`p`/`P` of several rows lands them together.** If any of them is a package, they land at a root boundary (`put_place` with that package); otherwise at the first line's place. The cursor goes to the first landed row.
5. **`J`/`K` over a selection is one `Edit::Move` of the neighbouring sibling** across the block, by the block's length. A count is ignored.
6. **The live step rolls back only while the step is the sheet's last change.** A tile `edit_seq` counter proves it. Any other edit (in practice a load, since every other verb closes the editor first) makes `escape` leave the steps in place and record them as one undo entry, so they stay undoable.

## Review Focus

1. **A package and its own legs both selected, with an open package.**
   - `V` over the package row and one of its legs, then a typed strike: each leg is written once.
   - A step: each leg moves by exactly one unit, not two.
   - The footer counts the package, not the package plus the leg.

   Pinned in Task 1 (`lines_of_dedupes_a_package_and_its_legs`, `top_most_drops_a_selected_packages_legs`) and in Task 5 (`a_typed_strike_over_a_package_and_its_leg_writes_each_leg_once`).
2. **A step that would make a qty zero.** `Edit::SetQty` refuses `ZeroQty`, so the whole press must write nothing. Pinned in Task 6 (`a_step_that_zeroes_a_qty_writes_nothing`).
3. **Undo after a bulk change.** One `u` must restore all of it, and one `ctrl+r` redo it, for the typed commit, the kept steps, `d`, `g u` and the multi-put. Each task's tests end with `u` and `ctrl+r`.
4. **A collapse hides the anchor.** `z c` on a package whose leg is the anchor must clear the selection with a notice, not re-anchor. Pinned in Task 2 (`collapsing_the_anchors_package_clears_the_selection`).
5. **Escape after steps, with deliveries arriving.** A pricing outcome for the stepped lines must not stop the rollback, because outcomes are not edits. Pinned in Task 6 (`escape_after_steps_rolls_back_even_after_prices_arrive`).

---

## File Structure

Create:
- `crates/geode-pricer/src/core/select.rs`: the pure selection rules over `Sheet`.
- `crates/geode-pricer/src/tile/select.rs`: the tile's selection doors and verbs.
- `crates/geode-pricer/src/tile/tests/selection.rs`: tile tests over the existing `Harness`.

Modify:
- `crates/geode-pricer/src/core/mod.rs`: `pub mod select;`.
- `crates/geode-pricer/src/tile.rs`:
  - fields `selection`, `resolved`, `selection_extent`, `totals` and `edit_seq`;
  - `key_context`/`mode`, `dispatch` arms and `after_edit`;
  - both sheet-replace sites;
  - `apply_edits` split into `apply_batch` plus record;
  - `register: Option<Vec<RowSpec>>`;
  - the editor hooks;
  - `mod select;` and, inside `mod tests`, `mod selection;`.
- `crates/geode-pricer/src/delegate.rs`: a `selected: Option<Resolved>` mirror with the tint, and the `CellPointer` emission (Task 7).
- `crates/geode-pricer/src/header.rs`: `render_footer` paints the aggregate strip while a selection is live.
- `crates/geode-pricer/src/content.rs`: normal-mode `v`/`shift+v`, the `mode == visual` block, and the `ACTIONS` rows.
- `crates/geode-pricer/README.md`, `docs/current/features.md` (Pricing and the line pricer), `docs/current/keymaps.md`, `scripts/mutation-check.sh` (Task 8).

Facts the tasks rely on (verified 2026-09-27):

- **Cursor and grid:**
  - `Cursor { line: Option<LineId>, col: usize /*plan col*/, last_row }` (tile.rs:136).
  - `GridRow { kind, row: Option<usize> /*sheet row*/, id: Option<LineId>, depth, tag, search, cells: Vec<GridCell{text, state}> }`, with `model.grid_row_of(id)`.
  - Tile helpers: `cursor_row()` gives the grid row; `cursor_sheet_row()`; `step_rows` (clamps); `set_cursor_row`; `sync_cursor(&self)`.
- **Columns:**
  - `plan.columns: Vec<PlannedColumn{def: ColumnDef{name, kind, editable, ..}, label, width, format}>`.
  - The delegate's table column 0 is the tree column (`TREE_COL`); `plan_col(col_ix) = col_ix - 1`.
- **Sheet:**
  - `parent(row)`, `children(row) -> Range<usize>`, `is_package(row)`, `is_line(row)`, `depth(row)`, `id(row)`, `index_of(id)`, `qty(row)`, `result(row) -> Option<&PriceResult>`, `state(row) -> &LineState`, `roots()`.
  - `shorthand(row)` (sheet.rs).
  - A package's `result` is the qty-weighted sum of its legs (`fold_packages`); a line's is its unit result.
- **Edits:**
  - `Sheet::apply(Edit) -> Result<Undo, EditError>` and `Sheet::undo(&Undo) -> Result<Undo, EditError>`.
  - `Edit::{Remove{at}, Insert{place, rows}, Move{row, delta /*siblings*/}, Group{first,count,template,id}, Ungroup{row}, SetQty, SetShift, SetInstrument}`.
  - `EditError::{NotContiguousRoots, MoveOffEnd, ZeroQty, ..}` with `Display`.
- **Tile edit doors:**
  - `apply_edit(edit, cx)`, and `apply_edits(edits, cx)`, which is one undo entry and rolls back on refusal.
  - `after_edit(cx)` rebuilds, submits pricing and arms the save.
  - Sheet replacement happens at tile.rs:~2150 (a load) and ~2969 (`Sheet::new`); both call `self.undo.clear()`.
- **Cell rules** (`core/cell.rs`):
  - `editor_for(&sheet, row, kind, &format) -> Result<CellEditor, &'static str>`, where `CellEditor::{Text(String), Choice{..}, Date(Option<NaiveDate>)}`.
  - `commit(&sheet, row, kind, text) -> Result<Option<Edit>, String>`.
  - `commit_date(&sheet, row, date) -> Result<Option<Edit>, String>`.
  - `nudge(kind, text, steps) -> Result<String, String>`.
  - `READ_ONLY = "read-only"`.
- **Clip** (`core/clip.rs`): `spec_of(&sheet, row) -> RowSpec` and `put_place(&sheet, Option<usize>, below, &RowSpec) -> Place`.
- **Test fixture and harness:**
  - `BOOK = ["SPX Z26 5000 C", "-5 SPX Z26 4800/5200 CS", "SPX Z26 4000 P"]`. The package starts closed, so the grid rows are 0, 1 and 2.
  - Openers: `open_seeded(cx, &BOOK)`.
  - Actions and prices: `h.dispatch(vcx, verb, count)`, `h.command`, `answer_all(h, vcx, price)`.
  - Reads: `h.mode`, `h.cursor -> Option<(grid row, plan col)>`, `h.cell(vcx, row, "col")`, `h.tree`, `h.footer`, `h.columns`.
  - Local helpers: `goto_column(h, vcx, "strike")`, `set_editor`, `editor_text`, `keys`, `can_undo`, `centre_of`, `click_at`, `h.draw`.
  - The clipboard is read with `vcx.update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()))`.
  - The cell selector is `pricer-cell-{row}-{col_ix}`; the editor selector is `pricer-editor-{r}-{c}`.

---

### Task 1: Pure selection rules over the sheet

**Files:**
- Create: `crates/geode-pricer/src/core/select.rs`
- Modify: `crates/geode-pricer/src/core/mod.rs`

**Interfaces (produces):**
```rust
pub fn lines_of(sheet: &Sheet, rows: &[usize]) -> Vec<usize>;             // leaf lines, deduped, sheet order
pub fn top_most(sheet: &Sheet, rows: &[usize]) -> Vec<usize>;             // rows with no selected ancestor, input order
pub fn group_plan(sheet: &Sheet, top: &[usize]) -> Result<(usize, usize), &'static str>; // (first, count)
pub fn move_plan(sheet: &Sheet, top: &[usize], down: bool) -> Result<Edit, &'static str>;
pub fn risk_totals(sheet: &Sheet, top: &[usize]) -> [Option<f64>; 6];   // price, delta, gamma, vega, theta, rho
pub const RISK: [ColumnKind; 6] = [Price, Delta, Gamma, Vega, Theta, Rho];
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Skip { ReadOnly, NotApplicable, NotNumeric, Refused }
#[derive(Debug, Clone, Default)] pub struct Skips(/* BTreeMap<Skip, usize> */);
impl Skips { pub fn add(&mut self, s: Skip); pub fn total(&self) -> usize; pub fn describe(&self) -> String }
pub fn set_notice(n: usize, skips: &Skips) -> String;
pub fn step_notice(n: usize, total_steps: i64, skips: &Skips) -> String;
```

- [ ] **Step 1: Write the failing tests** (inline `#[cfg(test)] mod tests`). Build sheets with the same helper `core/edit.rs`'s tests use (read its test module's sheet constructor, e.g. from shorthand lines, and reuse it).

```rust
#[test]
fn lines_of_dedupes_a_package_and_its_legs() {
    // roots: 0 line A; 1 package P (legs 2, 3); 4 line B
    let s = sheet_of(&["SPX Z26 5000 C", "-5 SPX Z26 4800/5200 CS", "SPX Z26 4000 P"]);
    assert_eq!(lines_of(&s, &[1]), vec![2, 3], "a package stands for its legs");
    assert_eq!(lines_of(&s, &[1, 2]), vec![2, 3], "its own leg is not edited twice");
    assert_eq!(lines_of(&s, &[0, 1, 4]), vec![0, 2, 3, 4]);
}

#[test]
fn top_most_drops_a_selected_packages_legs() {
    let s = sheet_of(&["SPX Z26 5000 C", "-5 SPX Z26 4800/5200 CS", "SPX Z26 4000 P"]);
    assert_eq!(top_most(&s, &[1, 2, 3, 4]), vec![1, 4]);
    assert_eq!(top_most(&s, &[2, 3]), vec![2, 3], "legs without their package stand alone");
}

#[test]
fn group_plan_needs_contiguous_root_lines_and_names_why() {
    let s = sheet_of(&["SPX Z26 5000 C", "SPX Z26 4000 P", "-5 SPX Z26 4800/5200 CS", "SPX Z26 3000 P"]);
    assert_eq!(group_plan(&s, &[0, 1]), Ok((0, 2)));
    assert_eq!(group_plan(&s, &[1, 2]), Err("can't group: selection includes a package"));
    assert_eq!(group_plan(&s, &[3]), Err("can't group: lines are inside a package"));
}

#[test]
fn move_plan_moves_the_neighbouring_sibling_across_the_block() {
    let s = sheet_of(&["SPX Z26 5000 C", "SPX Z26 4000 P", "SPX Z26 3000 P", "SPX Z26 2000 P"]);
    // block = roots 0..2; down: root 2 hops up over two siblings
    assert_eq!(move_plan(&s, &[0, 1], true), Ok(Edit::Move { row: 2, delta: -2 }));
    assert_eq!(move_plan(&s, &[2, 3], true), Err("cannot move past the end"));
    assert_eq!(move_plan(&s, &[1, 2], false), Ok(Edit::Move { row: 0, delta: 2 }));
}

#[test]
fn move_plan_refuses_a_selection_across_parents() {
    let s = sheet_of(&["SPX Z26 5000 C", "-5 SPX Z26 4800/5200 CS"]);
    assert_eq!(move_plan(&s, &[0, 2], true), Err("can't move: selection spans packages"));
}

#[test]
fn risk_totals_are_position_totals_and_refuse_an_incomplete_column() {
    let mut s = sheet_of(&["2 SPX Z26 5000 C", "SPX Z26 4000 P"]);
    s.set_result_for_tests(0, result(1.5)); // use the sheet's real result-install door
    s.set_result_for_tests(1, result(0.25));
    let t = risk_totals(&s, &[0, 1]);
    assert_eq!(t[0], Some(2.0 * 1.5 + 0.25), "qty × unit price per line");
    let unpriced = sheet_of(&["SPX Z26 5000 C", "SPX Z26 4000 P"]);
    assert_eq!(risk_totals(&unpriced, &[0, 1])[0], None, "no partial sum");
}

#[test]
fn notices_count_cells_and_name_each_skip_in_a_fixed_order() {
    let mut k = Skips::default();
    assert_eq!(set_notice(1, &k), "set 1 cell");
    k.add(Skip::Refused);
    k.add(Skip::ReadOnly);
    k.add(Skip::ReadOnly);
    assert_eq!(set_notice(6, &k), "set 6 cells, skipped 3 (2 read-only, 1 refused)");
    assert_eq!(step_notice(4, -3, &Skips::default()), "stepped 4 cells -3");
}
```

Replace `sheet_of`, `set_result_for_tests` and `result` with the real test-module helpers. `core/sheet.rs`'s and `core/edit.rs`'s tests build sheets and install results, for example through `Sheet::deliver` or a `#[cfg(test)]` door; use whichever exists and do not add production API for tests. The row indices assume the CS package occupies sheet rows 1, 2, 3; confirm that with `s.children(1)` and adjust the literals, keeping the intent.

- [ ] **Step 2: Run to see them fail**

Run `cargo test -p geode-pricer --lib -- core::select`. Expected: a compile error (the module doesn't exist yet).

- [ ] **Step 3: Implement**

```rust
//! What a grid selection reaches on the sheet: the lines an edit
//! writes (a package stands for its legs), the top-most rows a verb
//! or a total acts on (a package already carries its legs), the group
//! and move plans with their refusals, position risk totals, and the
//! one notice line that counts what a bulk edit wrote and skipped.

use crate::core::columns::ColumnKind;
use crate::core::edit::Edit;
use crate::core::sheet::{LineState, Sheet};
use std::collections::BTreeMap;

pub const RISK: [ColumnKind; 6] = [
    ColumnKind::Price, ColumnKind::Delta, ColumnKind::Gamma,
    ColumnKind::Vega, ColumnKind::Theta, ColumnKind::Rho,
];

/// The leaf lines under `rows`, each once, in sheet order: a line is
/// itself, a package is its legs whether it is open or not. A package
/// selected together with its own legs must never edit a leg twice.
pub fn lines_of(sheet: &Sheet, rows: &[usize]) -> Vec<usize> {
    let mut out: Vec<usize> = rows
        .iter()
        .flat_map(|&r| {
            if sheet.is_package(r) { sheet.children(r).collect::<Vec<_>>() } else { vec![r] }
        })
        .filter(|&r| sheet.is_line(r))
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// The rows of `rows` with no selected ancestor. A package already
/// carries its legs, so a verb or a total over both would double them.
pub fn top_most(sheet: &Sheet, rows: &[usize]) -> Vec<usize> {
    geode_core::grid::selection::top_most(rows, sheet.len(), |r| sheet.parent(r))
}

pub fn group_plan(sheet: &Sheet, top: &[usize]) -> Result<(usize, usize), &'static str> {
    if top.iter().any(|&r| sheet.is_package(r)) {
        return Err("can't group: selection includes a package");
    }
    if top.iter().any(|&r| sheet.parent(r).is_some()) {
        return Err("can't group: lines are inside a package");
    }
    let (Some(&first), Some(&last)) = (top.iter().min(), top.iter().max()) else {
        return Err("no row");
    };
    Ok((first, last - first + 1))
}

/// One `Edit::Move` that slides the whole block one sibling step: the
/// neighbouring sibling hops across the block by its length. Every
/// member must share a parent, or the block has no single sibling order.
pub fn move_plan(sheet: &Sheet, top: &[usize], down: bool) -> Result<Edit, &'static str> {
    let Some(&first) = top.first() else { return Err("no row") };
    let parent = sheet.parent(first);
    if top.iter().any(|&r| sheet.parent(r) != parent) {
        return Err("can't move: selection spans packages");
    }
    let siblings = sheet.siblings(first); // use the sheet's own sibling list (edit.rs move_row uses it)
    let pos = |r: usize| siblings.iter().position(|s| *s == r);
    let (Some(lo), Some(hi)) = (top.iter().filter_map(|&r| pos(r)).min(), top.iter().filter_map(|&r| pos(r)).max()) else {
        return Err("no row");
    };
    let len = (hi - lo + 1) as isize;
    if down {
        let next = *siblings.get(hi + 1).ok_or("cannot move past the end")?;
        Ok(Edit::Move { row: next, delta: -len })
    } else {
        let prev = *lo.checked_sub(1).and_then(|p| siblings.get(p)).ok_or("cannot move past the end")?;
        Ok(Edit::Move { row: prev, delta: len })
    }
}

/// Position totals over top-most rows: `qty × value` for a line, the
/// package's own folded sum (already qty-weighted) for a package. A
/// column with any row unpriced or failed is `None` — an incomplete
/// total would read as a real one.
pub fn risk_totals(sheet: &Sheet, top: &[usize]) -> [Option<f64>; 6] {
    let mut sums = [Some(0.0f64); 6];
    for &r in top {
        let weight = if sheet.is_package(r) { 1.0 } else { sheet.qty(r) as f64 };
        let value = match (sheet.state(r), sheet.result(r)) {
            (LineState::Failed(_), _) | (_, None) => None,
            (_, Some(v)) => Some(v),
        };
        let picks = value.map(|v| [v.price, v.delta, v.gamma, v.vega, v.theta, v.rho]);
        for (i, s) in sums.iter_mut().enumerate() {
            *s = match (*s, picks) { (Some(a), Some(p)) => Some(a + weight * p[i]), _ => None };
        }
    }
    sums
}
```

Complete this with `Skip` (phrases `read-only`, `n/a`, `not numeric` and `refused`, in that order), `Skips`, `set_notice` and `step_notice`. Copy their shape from `crates/geode-marketdata/src/core/bulk.rs`: `describe` gives `", skipped N (…)"` or `""`, and there is a private `cells(n)` plural helper. Use `sheet.siblings` if it is `pub(crate)`; if it is private, make it `pub(crate)` and add a one-line doc. Add `pub mod select;` to `core/mod.rs`.

- [ ] **Step 4: Run and pass.** Run `cargo test -p geode-pricer --lib -- core::select`.
- [ ] **Step 5: Commit.** Message: `feat(pricer): pure rules for grid selections over the sheet`, with the trailer `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

---

### Task 2: Selection state, keys, tint, footer totals, lost anchor

**Files:**
- Create: `crates/geode-pricer/src/tile/select.rs`, `crates/geode-pricer/src/tile/tests/selection.rs`
- Modify: `tile.rs`, `delegate.rs`, `header.rs`, `content.rs`

**Interfaces:**
- *Consumes:* `core::select::{top_most, risk_totals, RISK}`.
- *Produces* (on `PricerTile`):
  - Fields:
    - `selection: Option<Selection<LineId, &'static str>>`
    - `resolved: Option<Resolved>`
    - `selection_extent: Option<SharedString>`
    - `totals: Vec<AggregateCell>`
  - Methods:
    - `fn start_selection(&mut self, kind: SelectKind)`
    - `fn clear_selection(&mut self)`
    - `fn refresh_selection(&mut self) -> bool`, which returns whether the anchor was lost and the footer was set.
    - `fn selected_sheet_rows(&self) -> Vec<usize>`: the resolved grid rows' sheet rows, in grid order.
    - `fn selection_targets(&self) -> (Vec<usize> /*lines*/, Vec<usize> /*plan cols*/)`. `V` gives the cursor column; `v` gives the block columns.
    - `#[cfg(test)] pub(crate) fn resolved(&self) -> Option<&Resolved>`
  - Delegate: `pub(crate) selected: Option<Resolved>`.
  - `header::render_footer`: gains `extent: Option<&SharedString>, totals: &[AggregateCell]`.

- [ ] **Step 1: Write the failing tests.** Put them in `src/tile/tests/selection.rs`, declared as the last item inside `tile.rs`'s `mod tests` as `mod selection;`, and start with `use super::*;`.

```rust
use super::*;
use geode_core::grid::selection::SelectKind;

fn resolved(h: &Harness, vcx: &VisualTestContext) -> Option<(SelectKind, std::ops::Range<usize>, std::ops::Range<usize>)> {
    h.tile.read_with(vcx, |t, _| t.resolved().map(|r| (r.kind, r.rows.clone(), r.cols.clone())))
}

#[gpui::test]
fn v_starts_a_block_and_motions_extend_it(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    goto_column(&h, &mut vcx, "strike");
    let strike = h.cursor(&vcx).unwrap().1;
    h.dispatch(&mut vcx, "visual_block", None);
    assert_eq!(h.mode(&mut vcx), "visual");
    h.dispatch(&mut vcx, "down", Some(9)); // clamps
    h.dispatch(&mut vcx, "right", None);
    assert_eq!(resolved(&h, &vcx), Some((SelectKind::Block, 0..3, strike..strike + 2)));
}

#[gpui::test]
fn shift_v_switches_kind_keeping_the_anchor_and_the_same_key_clears(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    h.dispatch(&mut vcx, "visual_block", None);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "visual_rows", None);
    let cols = h.columns(&mut vcx).len();
    assert_eq!(resolved(&h, &vcx), Some((SelectKind::Rows, 0..2, 0..cols)));
    h.dispatch(&mut vcx, "visual_rows", None);
    assert_eq!(resolved(&h, &vcx), None);
    assert_eq!(h.mode(&mut vcx), "normal");
}

#[gpui::test]
fn escape_clears_only_the_selection_first(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "escape", None);
    assert_eq!(resolved(&h, &vcx), None);
    assert_eq!(h.mode(&mut vcx), "normal");
}

#[gpui::test]
fn collapsing_the_anchors_package_clears_the_selection(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "expand", None); // open the CS package
    h.dispatch(&mut vcx, "down", None);   // its first leg
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "up", None);
    // collapse through the palette-reachable verb: the leg anchor is hidden
    h.dispatch(&mut vcx, "collapse", None);
    assert_eq!(resolved(&h, &vcx), None);
    assert_eq!(h.footer(&vcx).as_deref(), Some("selection cleared: anchor row no longer shown"));
}

#[gpui::test]
fn the_footer_totals_position_risk_over_top_most_rows(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    answer_all(&h, &mut vcx, 1.0);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "bottom", None);
    let totals = h.tile.read_with(&vcx, |t, _| t.totals.clone());
    let price = totals.iter().find(|c| c.label.as_ref() == "price").expect("a price total");
    // Two 1-lot lines at 1.00, plus the package's own folded sum (read it off its cell).
    let package: f64 = h.cell(&vcx, 1, "price").parse().unwrap();
    let expected = 1.0 + 1.0 + package;
    assert_eq!(price.text.as_ref(), format!("{expected:.2}"));
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.selection_extent.clone()).as_deref(),
        Some(format!("3 rows × {} cols", h.columns(&mut vcx).len()).as_str()));
    h.draw(&mut vcx);
    assert!(vcx.debug_bounds("aggregate-extent").is_some(), "the strip paints in the footer");
}

#[gpui::test]
fn an_unpriced_row_turns_its_total_into_a_dash(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK); // no answers: nothing priced
    h.dispatch(&mut vcx, "visual_rows", None);
    let totals = h.tile.read_with(&vcx, |t, _| t.totals.clone());
    assert!(totals.iter().all(|c| c.text.as_ref() == "—" && c.refused));
}

#[gpui::test]
fn a_new_sheet_clears_the_selection(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.command(&mut vcx, "new").unwrap();
    assert_eq!(resolved(&h, &vcx), None);
}
```

Also add these to `content.rs`'s keymap tests, in the style of the existing ones:
- `v` and `shift+v` in normal mode map to `pricer::visual_block` and `pricer::visual_rows`;
- in `mode == visual`, `j`, `y`, `d`, `shift+j`, `shift+k`, `g p`, `g u`, `i`, `enter`, `v`, `shift+v` and `escape` resolve.

Before writing these, check three things and adjust the test lines, keeping each assertion's intent:
- the package's real sheet rows;
- the exact dispatch verbs for expand and collapse (`expand`/`collapse`);
- whether `h.command(&mut vcx, "new")` exists (use `open_with` or `new` if it is named differently).

- [ ] **Step 2: Run them and confirm they fail.**

- [ ] **Step 3: Implement.**

`content.rs`:
- In the normal block, add `"v" = "pricer::visual_block"` and `"shift+v" = "pricer::visual_rows"`.
- Add a `[[bindings]] context = "pricer && mode == visual"` block with:
  - the motion keys, copied from the normal block;
  - `"y" = "pricer::yank"`, `"d" = "pricer::delete"`, `"shift+j" = "pricer::move_down"`, `"shift+k" = "pricer::move_up"`, `"g p" = "pricer::group"`, `"g u" = "pricer::ungroup"`, `"i" = "pricer::edit"`, `"enter" = "pricer::edit"`, `"v" = "pricer::visual_block"`, `"shift+v" = "pricer::visual_rows"` and `"escape" = "pricer::escape"`.
- Add `yank` ("Yank selection"), `visual_rows` ("Select rows") and `visual_block` ("Select cells") to `ACTIONS`.
- Update the `DEFAULT_KEYMAP` doc comment: the visual context, and single-key verbs there.

`tile.rs`:
- Add `mod select;`.
- Add the four fields plus `edit_seq: u64`, which Task 6 uses; initialise them in `new`.
- Change `mode()`: if none of insert and menu hold and a selection is live, report `"visual"`. `key_context` adds `.pair("select", "rows"|"block")` whenever a selection is live.
- `sync_cursor` becomes `&mut self` and first runs `if self.refresh_selection() { self.rebuild_chrome() }`. Its delegate mirror also sets `d.selected = self.resolved.clone()`. Fix the callers.
- Dispatch arms:
  - `"visual_rows" | "visual_block"` call `start_selection`.
  - `"escape"`: when a selection is live, clear it first and return through the dispatch tail.
  - `"yank"` is a stub until Task 3: `self.footer = Some("select with V or v first".into())` when no selection is live.
- Clear the selection silently at both sheet-replace sites (tile.rs:~2150 and ~2969).
- `rebuild_chrome` keeps `footer_text` as it is. Render passes `extent`/`totals` to `render_footer` only while a selection is live AND `footer_text` is `None`, so a refusal notice wins.

`tile/select.rs`:
- Port Part 2's `start_selection` / `clear_selection` / `refresh_selection` shape (crates/geode-marketdata/src/tile/select.rs).
  - Row identity: `self.model.rows[i].id` (`None` rows cannot anchor, so `v` there refuses: `select from a line or package row`).
  - Column identity: `self.plan.columns[c].def.name`.
  - `col_count` is `self.plan.columns.len()`.
- `refresh_selection` then prepares `selection_extent` and `totals`:
  - Compute `top_most(&self.sheet, &self.selected_sheet_rows())`, then `risk_totals`.
  - For each `RISK` kind whose column is in the plan, push an `AggregateCell` with the plan column's label and `text` from `geode_core::format::format_number(v, &planned.format)`, or `"—"` with `refused: true`.
  - Kinds absent from the view get no cell.

`delegate.rs`:
- Add `selected: Option<Resolved>`.
- In `render_cell`, for plan columns, tint when `selected.contains(row_ix, plan_col)`. On the tree column, tint when `kind == Rows && contains_row(row_ix)`.
- Paint the tint as an absolute `inset_0` child before the text, `cx.theme().selection.opacity(0.35)`, so a package row's ground (painted by `render_tr`) still shows through and the cursor border stays on the cell.

`header.rs`:
- `render_footer(text, extent, totals, theme)`: when `text` is `None` and `extent` is `Some`, paint `aggregates::strip(extent, totals, &[], theme)` inside the same `FOOTER_HEIGHT` row. Otherwise paint today's danger line.

- [ ] **Step 4: Run and confirm they pass.** `cargo test -p geode-pricer` (the whole crate: existing footer and cursor tests must stay green).
- [ ] **Step 5: Commit** as `feat(pricer): V and v select rows and cell blocks, with risk totals`.

---

### Task 3: `y`, multi-row `p`/`P`, `d` over a selection

**Files:**
- Modify: `tile.rs`, `tile/select.rs`, `core/clip.rs` (only if `put_place` needs a multi-spec wrapper)
- Test: `tile/tests/selection.rs`

**Interfaces:**
- `register: Option<Vec<RowSpec>>`. `y y` and `d d` store `vec![spec]`.
- `fn yank_selection(&mut self, cx) -> Result<(), String>`
- `fn delete_selection(&mut self, cx) -> Result<(), String>`
- `put` places every spec in the register.

- [ ] **Step 1: Write the failing tests**

```rust
#[gpui::test]
fn y_over_rows_copies_top_most_shorthand_and_p_puts_them_all(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "yank", None);
    let clip = vcx.update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()));
    assert_eq!(clip.as_deref(), Some("SPX Z26 5000 C\n-5 SPX Z26 4800/5200 CS"));
    assert_eq!(h.mode(&mut vcx), "normal", "y ends the selection");
    h.dispatch(&mut vcx, "bottom", None);
    h.dispatch(&mut vcx, "put_below", None);
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.sheet.roots().count()), 5, "both rows landed");
    h.dispatch(&mut vcx, "undo", None);
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.sheet.roots().count()), 3, "one undo takes both back");
}

#[gpui::test]
fn y_over_a_block_copies_its_columns_as_tsv_and_keeps_the_register(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    goto_column(&h, &mut vcx, "strike");
    h.dispatch(&mut vcx, "visual_block", None);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "yank", None);
    let clip = vcx.update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text())).unwrap();
    let label = h.tile.read_with(&vcx, |t, _| {
        let c = t.cursor.col; t.plan.columns[c].label.to_string()
    });
    assert_eq!(clip, format!("{label}\n{}\n{}", h.cell(&vcx, 0, "strike"), h.cell(&vcx, 1, "strike")));
    assert!(h.tile.read_with(&vcx, |t, _| t.register.is_none()));
}

#[gpui::test]
fn d_over_rows_deletes_them_in_one_undo_entry(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "delete", None);
    assert_eq!(h.tree(&vcx), vec!["SPX Z26 4000 P".to_string()]);
    assert_eq!(h.mode(&mut vcx), "normal");
    h.dispatch(&mut vcx, "undo", None);
    assert_eq!(h.tree(&vcx).len(), 3, "one undo restores both, the package with its legs");
    h.dispatch(&mut vcx, "redo", None);
    assert_eq!(h.tree(&vcx).len(), 1);
}

#[gpui::test]
fn row_verbs_in_a_block_refuse_and_name_v(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    h.dispatch(&mut vcx, "visual_block", None);
    for (verb, text) in [
        ("delete", "d deletes rows — use V"),
        ("move_down", "shift+j/k move rows — use V"),
        ("group", "g p groups rows — use V"),
        ("ungroup", "g u ungroups rows — use V"),
    ] {
        h.dispatch(&mut vcx, verb, None);
        assert_eq!(h.footer(&vcx).as_deref(), Some(text), "{verb}");
        assert_eq!(h.mode(&mut vcx), "visual", "a refusal keeps the selection");
    }
    assert_eq!(h.tree(&vcx).len(), 3);
}
```

The `h.tree` strings are the grid rows' search text. Check how a closed package reads in `h.tree` (the existing tests show `"-5 SPX Z26 4800/5200 CS"` for the package row) and adjust the expected vectors to the real text.

- [ ] **Step 2: Run to see them fail.**

- [ ] **Step 3: Implement**
- **Register type:** change it to `Option<Vec<RowSpec>>`. Fix `y y` (tile.rs:~2285), `delete_row` (`vec![spec]`), and every other `register` user, including the test at tile.rs:~4549.
- **`put`:**
  - Take the specs: `let specs = self.register.clone().filter(|v| !v.is_empty()).ok_or("nothing to put")?`.
  - Choose the place: `let lead = specs.iter().find(|s| matches!(s, RowSpec::Package { .. })).unwrap_or(&specs[0]); let place = put_place(&self.sheet, self.cursor_sheet_row(), below, lead);`.
  - Apply `Edit::Insert { place, rows: specs.clone() }`. Keep today's parent-open and cursor logic, keyed to the first landed row, and open every landed package.
- **`yank_selection`:**
  - `Rows`: clipboard gets the top-most rows' `sheet.shorthand(r)` joined by `"\n"`; `register = Some(specs of top-most)`.
  - `Block`: clipboard gets the header of the block's plan-column labels, then each resolved grid row's `model.rows[r].cells[c].text` for the block columns, tab-joined. Check whether `GridRow.cells` is indexed by plan column; confirm in `grid.rs`. The register is untouched.
  - Both kinds call `clear_selection()` afterwards.
- **`delete_selection`:**
  - A `Block` refuses. `Rows`: `top = top_most(sheet rows)`.
  - Build the edits: `top` sorted descending, each an `Edit::Remove { at }`, so earlier indices stay valid. Apply them with `apply_edits`, one entry.
  - Put the register back in top-most sheet order, with each spec computed BEFORE the removes.
  - `clear_selection()`, and set the notice `deleted N rows`. The footer is a refusal channel, so show the count only if the tile has an info notice (`self.notice`); otherwise stay silent.
- **Routing in `dispatch`:** in the structural block, route `delete`, `move_*`, `group` and `ungroup` to their selection forms when `self.selection.is_some()`. The kind-mismatch refusals live in one helper: `fn row_verb_refusal(&self, verb: &str) -> Option<&'static str>`.

- [ ] **Step 4: Run and pass.** `cargo test -p geode-pricer` (including `dd_then_u_…`, `p_puts_…`, `yy_yanks_…`).
- [ ] **Step 5: Commit** — `feat(pricer): y, p and d act on a selection`.

---

### Task 4: `J`/`K`, `g p`, `g u` over a selection

**Files:** `tile/select.rs`, `tile.rs` (dispatch routing only); test in `tile/tests/selection.rs`.

**Interfaces:** `fn move_selection(&mut self, down: bool, cx) -> Result<(), String>`, `fn group_selection(&mut self, cx) -> Result<(), String>`, `fn ungroup_selection(&mut self, cx) -> Result<(), String>`.

- [ ] **Step 1: Write the failing tests**

```rust
#[gpui::test]
fn shift_j_moves_the_selected_block_as_a_unit_and_keeps_the_selection(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &["SPX Z26 5000 C", "SPX Z26 4000 P", "SPX Z26 3000 P"]);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "move_down", None);
    assert_eq!(h.tree(&vcx), vec!["SPX Z26 3000 P", "SPX Z26 5000 C", "SPX Z26 4000 P"]);
    assert_eq!(resolved(&h, &vcx).map(|r| r.1), Some(1..3), "the selection followed its lines");
    h.dispatch(&mut vcx, "move_down", None);
    assert_eq!(h.footer(&vcx).as_deref(), Some("cannot move past the end"));
    h.dispatch(&mut vcx, "undo", None);
    assert_eq!(h.tree(&vcx)[0], "SPX Z26 5000 C");
}

#[gpui::test]
fn g_p_over_root_lines_groups_them_and_u_restores(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &["SPX Z26 5000 C", "SPX Z26 4000 P", "SPX Z26 3000 P"]);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "group", None);
    assert_eq!(h.tree(&vcx)[0], "CUSTOM SPX Z26");
    assert_eq!(h.mode(&mut vcx), "normal", "g p ends the selection");
    h.dispatch(&mut vcx, "undo", None);
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.sheet.roots().count()), 3);
}

#[gpui::test]
fn g_p_names_why_it_refuses(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "down", None); // includes the CS package
    h.dispatch(&mut vcx, "group", None);
    assert_eq!(h.footer(&vcx).as_deref(), Some("can't group: selection includes a package"));
}

#[gpui::test]
fn g_u_ungroups_every_selected_package_in_one_undo_entry(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &["-5 SPX Z26 4800/5200 CS", "SPX Z26 5000 C", "2 SPX Z26 4000/3800 PS"]);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "bottom", None);
    h.dispatch(&mut vcx, "ungroup", None);
    assert!(h.tile.read_with(&vcx, |t, _| (0..t.sheet.len()).all(|r| !t.sheet.is_package(r))));
    h.dispatch(&mut vcx, "undo", None);
    assert_eq!(h.tile.read_with(&vcx, |t, _| (0..t.sheet.len()).filter(|&r| t.sheet.is_package(r)).count()), 2);
}

#[gpui::test]
fn g_u_with_no_package_selected_refuses(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &["SPX Z26 5000 C", "SPX Z26 4000 P"]);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "ungroup", None);
    assert_eq!(h.footer(&vcx).as_deref(), Some("no package selected"));
}
```

Check that the put-spread shorthand (`PS`) parses through the fixture's template set. If it doesn't, use two `CS` packages.

- [ ] **Step 2: Run to see them fail.**

- [ ] **Step 3: Implement**
- **`move_selection`:**
  - `let top = top_most(..)`.
  - `let edit = move_plan(&self.sheet, &top, down)?`.
  - Apply it with `apply_edit(edit, cx)`.
  - Keep the selection: it is anchored by `LineId`, and `sync_cursor` re-resolves it.
- **`group_selection`:**
  - `group_plan`, then `Edit::Group { first, count, template: Template::CUSTOM, id: None }` through `apply_edit`.
  - Expand the new package and move the cursor to it, as today's `group` does.
  - `clear_selection()`.
- **`ungroup_selection`:**
  - The packages are the top-most selected rows with `is_package`. None gives `Err("no package selected")`.
  - Build `Edit::Ungroup { row }` for each, descending, and apply them with `apply_edits`.
  - `clear_selection()`.
- **Dispatch routing:** a count is ignored while a selection is live.

- [ ] **Step 4: Run to see them pass.**
- [ ] **Step 5: Commit** as `feat(pricer): shift+j/k, g p and g u act on a selection`.

---

### Task 5: A typed value commits to every target line

**Files:** `tile.rs` (`begin_edit`, `commit_edit`, `commit_date`, the choice commit), `tile/select.rs`; tests.

**Interfaces:**
- `fn commit_selection(&mut self, text: &str, date: Option<NaiveDate>, window, cx) -> bool` returns `true` when the editor should close.
- The targets come from `selection_targets()`. `V` uses the cursor's plan column, `v` the block's plan columns, and the lines are always `lines_of(selected sheet rows)`.

- [ ] **Step 1: Write the failing tests**

```rust
#[gpui::test]
fn i_over_rows_writes_the_cursor_column_on_every_target_line_in_one_undo(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    goto_column(&h, &mut vcx, "strike");
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "bottom", None);
    h.dispatch(&mut vcx, "edit", None);
    set_editor(&h, &mut vcx, "4500");
    h.dispatch(&mut vcx, "commit", None);
    // 5000 C, both CS legs, 4000 P → all 4500; qty untouched
    let strikes = h.tile.read_with(&vcx, |t, _| {
        (0..t.sheet.len()).filter(|&r| t.sheet.is_line(r)).map(|r| t.sheet.shorthand(r)).collect::<Vec<_>>()
    });
    assert!(strikes.iter().all(|s| s.contains("4500")), "{strikes:?}");
    assert_eq!(h.mode(&mut vcx), "visual", "a commit keeps the selection");
    h.dispatch(&mut vcx, "escape", None);
    h.dispatch(&mut vcx, "undo", None);
    assert_eq!(h.cell(&vcx, 0, "strike"), "5000", "one undo takes every write back");
}

#[gpui::test]
fn a_typed_strike_over_a_package_and_its_leg_writes_each_leg_once(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "expand", None);
    goto_column(&h, &mut vcx, "strike");
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "down", None); // the package + its first leg
    h.dispatch(&mut vcx, "edit", None);
    set_editor(&h, &mut vcx, "4900");
    h.dispatch(&mut vcx, "commit", None);
    assert!(h.footer(&vcx).is_none() || !h.footer(&vcx).unwrap().contains("refused"));
    assert!(h.tile.read_with(&vcx, |t, _| t.undo.can_undo()));
    // exactly the two legs changed, once each: undo once restores both
    h.dispatch(&mut vcx, "escape", None);
    h.dispatch(&mut vcx, "undo", None);
    assert_eq!(h.cell(&vcx, 1, "strike"), "4800/5200");
}

#[gpui::test]
fn a_block_commit_skips_read_only_and_inapplicable_cells_and_counts_them(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    goto_column(&h, &mut vcx, "strike");
    h.dispatch(&mut vcx, "visual_block", None);
    h.dispatch(&mut vcx, "last_col", None); // strike … the read-only risk columns
    h.dispatch(&mut vcx, "edit", None);
    set_editor(&h, &mut vcx, "4500");
    h.dispatch(&mut vcx, "commit", None);
    let notice = h.tile.read_with(&vcx, |t, _| t.notice.clone()).map(|s| s.to_string());
    assert!(notice.as_deref().is_some_and(|n| n.starts_with("set ") && n.contains("read-only")), "{notice:?}");
}

#[gpui::test]
fn nothing_accepting_refuses_with_the_editor_open(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    goto_column(&h, &mut vcx, "strike");
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "edit", None);
    set_editor(&h, &mut vcx, "abc");
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(h.mode(&mut vcx), "insert");
    assert!(!h.tile.read_with(&vcx, |t, _| t.undo.can_undo()));
}
```

Find where the tile shows info (as opposed to refusal) messages; the entry's research says `self.notice`. Use the channel the tile really has. If it only has the footer, put `set N cells…` there and adjust the assertions. Confirm the package strike cell's text is `4800/5200` at `h.cell(&vcx, 1, "strike")`.

- [ ] **Step 2: Run the tests and see them fail.**

- [ ] **Step 3: Implement.**

Add `commit_selection` to `tile/select.rs`.
1. **Gates.** Return early if `self.loading`.
2. **Targets.** Call `selection_targets()`.
3. **Judge each target.** For each `line` × `col`:
   - Let `kind = plan.columns[col].def.kind`.
   - If `!def.editable`, skip it as `ReadOnly`.
   - If `kind == Expiry` and `date.is_some()`, the edit comes from `cell::commit_date(&sheet, line, date)`. Otherwise it comes from `cell::commit(&sheet, line, kind, text)`.
   - Map `Err(e)` to a skip: `e == READ_ONLY` gives `ReadOnly`; a barrier on a vanilla line gives `NotApplicable` (match the string `cell` returns there); anything else gives `Refused`.
   - `Ok(Some(edit))` is pushed. `Ok(None)` means the value is unchanged: count it as set, but push no edit.
4. **Nothing accepted.** If no target accepted, set the footer to `no selected cell accepts '{text}'{skips}`, return `false`, and keep the editor open.
5. **Apply.** Pass every edit to `apply_edits`, which makes one undo entry. The edits are `Set*`, so row indices are stable.
6. **Refused batch.** If `apply_edits` refuses (for example `ZeroQty` from typing `0` in qty), put the error in the footer and return `false`.
7. **Success.** Set the notice to `set_notice(n, &skips)` and return `true`.

Add the hooks, each as the first thing inside its commit path and guarded by `self.selection.is_some()`:
- The text editor's commit in `commit_edit`: read the input text, then `if self.commit_selection(&text, None, window, cx) { self.close_editor(window, cx) }` and return.
- The choice editor's commit: pass the picked option's text in the same way.
- `commit_date`: pass `(&date.format("%Y-%m-%d").to_string(), Some(date))`.

Opening:
- `begin_edit` under a selection opens on the cursor cell exactly as today.
- If the cursor cell's own editor refuses (for example a read-only column), still refuse, with today's message.
- Record the ruling in the doc comment: the cursor cell must itself be editable.

- [ ] **Step 4: Run the tests and see them pass.** Every existing editor test must stay green.
- [ ] **Step 5: Commit** with `feat(pricer): one typed value commits to every selected line`.

---

### Task 6: Live relative arrow step; one `escape` rolls it back

**Files:** `tile.rs` (`Editor::Text` gains `bulk`, `nudge`, `commit_edit`, `close_editor`, `after_edit`, the sheet-replace sites, `apply_edits` split), `tile/select.rs`; tests.

**Interfaces:**
- `fn apply_batch(&mut self, edits: Vec<Edit>) -> Result<Option<Undo>, EditError>`. This is the refactored body of `apply_edits`: it applies, rolls back on refusal and returns the combined inverse, but does not record or rebuild. `apply_edits` becomes `apply_batch`, then `record`, then `after_edit`.
- `edit_seq: u64` is bumped in `after_edit` and at both sheet-replace sites.
- The bulk state:
  ```rust
  struct Bulk {
      seeded: String,
      steps: i64,
      undo: Undo,
      seq: u64,
  }
  ```
  - `seeded` is the text the tile last put in the editor.
  - `undo` holds the inverses of every step since `i`, last first.
  - `seq` is the `edit_seq` after the last landed step.
- `fn bulk_step(&mut self, steps: i64, window, cx) -> Option<()>`. It returns `None` when the step doesn't apply: no selection, text typed, or a non-steppable cursor column.
- `fn settle_bulk(&mut self, keep: bool)`.
  - With `keep` (`enter` on untouched text), it records `bulk.undo` as ONE entry.
  - Without `keep`, it rolls back through `sheet.undo(&bulk.undo)`, then calls `after_edit`. It does this only while `bulk.seq == self.edit_seq`.
  - If `seq` differs, it records instead, so the steps stay undoable.

- [ ] **Step 1: Write the failing tests**

```rust
fn step_setup(h: &Harness, vcx: &mut VisualTestContext) {
    goto_column(h, vcx, "strike");
    h.dispatch(vcx, "visual_rows", None);
    h.dispatch(vcx, "bottom", None);
    h.dispatch(vcx, "edit", None);
}

#[gpui::test]
fn arrows_step_every_target_line_live_and_enter_keeps_them_as_one_undo(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    step_setup(&h, &mut vcx);
    h.dispatch(&mut vcx, "insert_up", Some(2));
    assert_eq!(h.cell(&vcx, 0, "strike"), "5002", "the grid paints the step at once");
    assert_eq!(h.cell(&vcx, 1, "strike"), "4802/5202", "each leg by one unit per press");
    assert_eq!(h.cell(&vcx, 2, "strike"), "4002");
    assert!(!h.tile.read_with(&vcx, |t, _| t.undo.can_undo()), "nothing recorded mid-edit");
    h.dispatch(&mut vcx, "commit", None);
    assert!(h.tile.read_with(&vcx, |t, _| t.undo.can_undo()));
    h.dispatch(&mut vcx, "escape", None);
    h.dispatch(&mut vcx, "undo", None);
    assert_eq!(h.cell(&vcx, 0, "strike"), "5000", "one undo takes every step back");
}

#[gpui::test]
fn escape_rolls_every_step_back(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    step_setup(&h, &mut vcx);
    h.dispatch(&mut vcx, "insert_up_big", None);
    h.dispatch(&mut vcx, "cancel", None);
    assert_eq!(h.cell(&vcx, 0, "strike"), "5000");
    assert_eq!(h.cell(&vcx, 1, "strike"), "4800/5200");
    assert!(!h.tile.read_with(&vcx, |t, _| t.undo.can_undo()), "a rolled-back step leaves no history");
}

#[gpui::test]
fn escape_after_steps_rolls_back_even_after_prices_arrive(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    answer_all(&h, &mut vcx, 1.0);
    step_setup(&h, &mut vcx);
    h.dispatch(&mut vcx, "insert_up", None);
    answer_all(&h, &mut vcx, 2.0); // outcomes for the stepped lines
    h.dispatch(&mut vcx, "cancel", None);
    assert_eq!(h.cell(&vcx, 0, "strike"), "5000");
}

#[gpui::test]
fn a_step_that_zeroes_a_qty_writes_nothing(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &["SPX Z26 5000 C", "2 SPX Z26 4000 P"]);
    goto_column(&h, &mut vcx, "qty");
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "edit", None);
    h.dispatch(&mut vcx, "insert_down", None); // 1 → 0 refuses; 2 → 1 must not land either
    assert_eq!(h.cell(&vcx, 1, "qty"), "2");
    assert!(h.footer(&vcx).is_some(), "the refusal is shown");
}

#[gpui::test]
fn after_typing_arrows_nudge_only_the_text(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    step_setup(&h, &mut vcx);
    set_editor(&h, &mut vcx, "4100");
    h.dispatch(&mut vcx, "insert_up", None);
    assert_eq!(editor_text(&h, &vcx).as_deref(), Some("4101"));
    assert_eq!(h.cell(&vcx, 0, "strike"), "5000", "typed: nothing live");
}

#[gpui::test]
fn typing_after_steps_replaces_them_in_one_undo(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    step_setup(&h, &mut vcx);
    h.dispatch(&mut vcx, "insert_up", None);
    set_editor(&h, &mut vcx, "4500");
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(h.cell(&vcx, 0, "strike"), "4500");
    h.dispatch(&mut vcx, "escape", None);
    h.dispatch(&mut vcx, "undo", None);
    assert_eq!(h.cell(&vcx, 0, "strike"), "5000", "one undo: the steps were rolled back before the write");
    assert!(!h.tile.read_with(&vcx, |t, _| t.undo.can_undo()));
}
```

Before writing these, check two things and adjust the values while keeping the intent:
- The step unit per strike is `cell::nudge`'s rule, one unit of the written precision, so `5000` steps to `5001`. Check `nudge_text` with `None` precision.
- The editor helpers' real names.

- [ ] **Step 2: Run these tests and see them fail.**

- [ ] **Step 3: Implement**
- **Split `apply_edits`:** move its body into `apply_batch`, so `apply_edits` becomes `apply_batch` + `record` + `after_edit`. The existing behaviour is unchanged, and the existing multi-edit tests prove it.
- **`edit_seq`:** bump it in `after_edit` and at both sheet-replace sites.
- **`Editor::Text`** gains `bulk: Option<Bulk>`. Set it in `begin_edit` when all of these hold:
  - a selection is live;
  - the editor opened is `Text`;
  - the cursor column's kind is one of Qty, Strike, Barrier, SpotShift or VolShift (the kinds `cell::nudge` steps).

  Seed it with `seeded = <opened text>`, `steps = 0`, `undo = Undo { inverse: vec![] }` and `seq = self.edit_seq`. Initialise the field as `None` at every construction site.
- **`nudge`:** first do `if self.bulk_step(steps, window, cx).is_some() { return }`.
- **`bulk_step`** in `tile/select.rs`:
  1. **Guards:**
     - the editor is `Text` and has a `bulk`;
     - a selection is live;
     - the input value equals `bulk.seeded`;
     - the cursor's kind is steppable.
  2. **Compute:** for each target `(line, col)`:
     - non-editable → `ReadOnly`;
     - `cell::editor_for` must be `Text(t)`, else `NotNumeric`;
     - `cell::nudge(kind, &t, steps)`: `Err` → `NotNumeric`;
     - `cell::commit(&sheet, line, kind, &next)`: `Ok(Some(e))` → push, `Ok(None)` → nothing, and `Err(e)` maps as in Task 5.
  3. **No edits:** set the footer to `no cells to step{skips}` and return `Some(())`.
  4. **Apply:** `apply_batch(edits)`. On `Err`, set the footer to the error and return `Some(())`: all or nothing, `bulk` untouched. On `Ok(Some(u))`:
     - prepend `u.inverse` to `bulk.undo.inverse`, so the last step undoes first;
     - `after_edit(cx)`;
     - `bulk.seq = self.edit_seq`, `bulk.steps += steps`;
     - reseed the editor text from `editor_for` on the cursor cell's sheet row, and set `bulk.seeded` to it;
     - set the notice to `step_notice(..)`.
- **`commit_edit`, untouched `enter`:** if `bulk` exists and the text equals `seeded`, take `bulk`, `settle_bulk(keep = true)`, close the editor, and return.
- **Typed commit with bulk:** the text differs from `seeded` and the bulk has stepped.
  1. Take `bulk`.
  2. If `bulk.seq == edit_seq`, roll the steps back (`sheet.undo(&bulk.undo)` + `after_edit`); otherwise record them first.
  3. Then call `commit_selection` (Task 5).
- **`close_editor`:** before the blur, take any `bulk` with `steps != 0` and call `settle_bulk(keep = false)`. Every cancel path goes through it: `cancel`, a click elsewhere, any verb (dispatch closes the editor first), and the menu.
- **Throttling:** each step calls `after_edit`, which reprices through the ordinary path. The tile's in-flight bookkeeping already supersedes an older batch, and its tag drops the older outcome. Add no throttling.

- [ ] **Step 4: Run and pass.** Run the whole crate: existing nudge and editor tests must stay green.
- [ ] **Step 5: Commit.** Message: `feat(pricer): arrows step a selection live; escape rolls the steps back`.

---

### Task 7: Mouse — shift+click and drag

**Files:** `delegate.rs`, `tile.rs` (the subscription and `pointer`), `tile/select.rs`; tests.

**Interfaces:**
- `delegate::CellPointer::{Press { row, col: Option<usize> /*plan col; None = tree cell or gutter*/, shift }, Drag { row, col: Option<usize>, tree: bool }}`
- `impl EventEmitter<CellPointer> for TableState<SheetDelegate>`
- `PricerTile::pointer(&mut self, e: CellPointer, window, cx)`

Port `crates/geode-marketdata/src/delegate.rs` (`wire_pointer`, `drag_origin`, `drag_last`, `editor_press`, the `render_tr` filler press) and `crates/geode-marketdata/src/tile.rs`'s `pointer` and editor-cell guards. Two differences from market data:
- The tree cell and the pricer gutter are the row handles. Market data used the row-label cell and its gutter.
- `on_table_event`'s `SelectCell` handles a `click_anchor`/`pressed` hand-off. Read it first and keep it intact.

- [ ] **Step 1: Write the failing tests**
  - A shift press on `pricer-cell-2-{strike+1}` with the cursor on (0, strike) gives `Block` over (0..3, strike..strike+2). Press with `Modifiers { shift: true }` and release. After that, a dispatched `yank` still reaches the tile.
  - A plain click clears the selection and moves the cursor.
  - A drag from `pricer-cell-0-{c}` to `pricer-cell-2-{c}` gives `Block`. A drag from the tree cell `pricer-cell-0-0` to `pricer-cell-2-0` gives `Rows`.
  - A shift press on the tree cell of row 2 gives `Rows` 0..3.
  - A click inside the open editor (`pricer-editor-{r}-{c}`) keeps it open, and with a stepped bulk it keeps the steps.
  - Clicking a package's chevron with a selection live toggles the package and does not start a selection. The chevron already stops propagation; the test proves it.

  Use market data's `drag` and `shift_press` test helpers as templates (`crates/geode-marketdata/src/tile/tests/selection.rs`).
- [ ] **Step 2: Run the tests and see them fail.**
- [ ] **Step 3: Implement** the port. Record the ordering-probe result (a press on mouse-down, `SelectCell` on the click) in `pointer`'s doc comment.
- [ ] **Step 4: Run the tests and see them pass.** Every existing click, double-click and chevron test must stay green.
- [ ] **Step 5: Commit** with `feat(pricer): shift+click and drag select on the sheet`.

---

### Task 8: Docs, mutation entries, gates

**Files:** `crates/geode-pricer/README.md`, `docs/current/features.md` (`## Pricing and the line pricer`: extend the normal-mode keys table and add a "Selection" subsection), `docs/current/keymaps.md`, `scripts/mutation-check.sh`.

- [ ] **Step 1: Update the docs.**
  - Each sentence must be checked against the code; doc copy is a behaviour claim.
  - Cover the keys and the `visual` context, and every verb's behaviour and refusal.
  - Cover the rule that edits act on lines, and the `V`-column rule.
  - Cover the live step: `enter` keeps it as one undo entry, and `escape` rolls it back.
  - Cover the footer position totals and `—`.
  - Cover the mouse.
  - State the plan's six rulings as current behaviour.
  - Limitations: contiguous selections only, no paste of a TSV block, and a count ignored on `J`/`K`/`g p` in visual mode.
- [ ] **Step 2: Add mutation entries.** Use the `pricer select:` area prefix and the file's `run_mutation` form, one entry per contract:
  1. `lines_of` dedupes, so a package plus its leg writes each leg once.
  2. `top_most` for totals, so a package plus its legs is not double counted.
  3. Position weighting (`qty ×`) in `risk_totals`.
  4. An incomplete total is `—`.
  5. The block refusal for `d`.
  6. One undo entry for `d` over rows (`apply_edits` rather than a loop of `apply_edit`).
  7. `move_plan`'s single neighbouring move.
  8. `group_plan`'s package refusal.
  9. The `V` column rule (cursor column only).
  10. The escape rollback (`settle_bulk(false)`).
  11. The `edit_seq` guard.
  12. Step all-or-nothing (`apply_batch` rollback).
  13. The editor-cell press guard.
  14. Sheet replace clears the selection.

  Verify each with `zsh scripts/mutation-check.sh "pricer select"`; every entry must be `caught`.
- [ ] **Step 3: Run the gates.** Run them in the foreground with a generous timeout, and never poll with pgrep.
  ```
  cargo fmt --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  cargo check -p geode-shell --features test-support --all-targets
  zsh scripts/mutation-check.sh --anchors-only
  ```
- [ ] **Step 4: Commit** with `docs(pricer): selections, bulk edits, live step and risk totals; mutation entries`.
- [ ] **Step 5: Display-check list for Matthew:**
  - the tint over package ground and stale or failed cells, with the cursor border inside;
  - the footer strip with totals and `—`;
  - the live step repaint and repricing under a held `shift+up`;
  - shift+click and drag;
  - a click inside an open editor;
  - light and dark themes.
