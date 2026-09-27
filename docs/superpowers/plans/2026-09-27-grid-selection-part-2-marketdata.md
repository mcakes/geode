# Grid selection Part 2 — market-data panel Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `V` (rows) and `v` (cell block) selections on the market-data panels (CVI, Dividend), with `y`, `d`, `:bump`, a one-value commit to every accepting cell, and a live relative arrow step over the selection that one `escape` undoes.

**Architecture:** The shared pure core `geode_core::grid::selection` (Part 1, merged) supplies `Selection`, `resolve_with`, `Resolved` and `Lost`. Pure market-data rules go into a new `core/bulk.rs` (per-kind acceptance, per-column step size, skip counting and notices), `core/cursor.rs` (`step_clamped`) and `core/draft.rs` (`Draft::restore_from`). The tile-side state and verbs live in a new child module `src/tile/select.rs` (an `impl MarketDataTile` block; a child module can see the tile's private fields), so `tile.rs` (14k lines) only gains hooks. The delegate mirrors a `Resolved` for the tint, exactly as it mirrors the cursor.

**Tech Stack:** Rust, GPUI + gpui-component 0.6.2 `DataTable`, `geode_core::grid::selection`, `geode_shell::keymap`, `geode_shell::shell::aggregates::strip`.

**Spec:** `docs/superpowers/specs/2026-09-26-grid-selection-design.md` — §4.1 lifecycle, §4.2 common verbs, §4.4 market-data (with the 2026-09-27 live-step ruling), §4.6 kind mismatch, §5 mouse, §6 keymaps, §7 testing. Part 1's plan (`docs/superpowers/plans/2026-09-26-grid-selection-part-1-blotter.md`) and the blotter code are the reference implementation for keys, tint, pointer wiring and the lost-anchor notice.

## Global Constraints

- `V` = `SelectKind::Rows`, `v` = `SelectKind::Block`. The other key switches kind and keeps the anchor. The same key again clears (`start_selection`'s rule, as in `crates/geode-blotter/src/delegate.rs:533`).
- `escape` clears only the selection. A second `escape` does today's `escape` (find, notice).
- While a selection is live, motions clamp at the edges (`vimnav::apply_clamped`) and never enter the header attribute strip. `k` at row 0 clamps.
- Consuming verbs (`y`, `d`) end the selection. Repeatable edits (arrow step, `:bump`, block commit) keep it.
- Anchor identity: the row is `RowModel::label` and the column is `MatrixModel::columns[i]`. A lost anchor clears the selection with the notice `selection cleared: anchor row no longer shown` or `selection cleared: anchor column no longer shown`. No nearest-row guess.
- The header attribute strip and the row-label column are never selection members.
- A `Rows` selection's editable members skip the leading `model.slice_columns` (fwd/atm/skew), which is `:bump row`'s rule. A `Block` is exactly its rectangle.
- In visual mode the verbs are single keys (`y`, `d`). The doubled forms (`y y`, `y c`, `d d`) stay normal-mode only.
- The gates are `:bump`'s: `held_refusal()` then `edit_base()`.
- Notices: `set N cells[, skipped M (…)]`, `stepped N cells ±S[, skipped M (…)]`, `d deletes rows — use V`.
- Selection tint is `theme.selection.opacity(0.35)`. The cursor keeps `table_active_border` inside the tint.
- The UI thread does no unbounded work in render: `Resolved` and the footer extent text are prepared at change points, and render only looks them up.
- Every new test goes through a production route: `h.dispatch` (the `TileContent` door), `h.command`, or real mouse events at painted bounds.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test -p geode-marketdata` pass after every task.

### Rulings made in this plan (surface them to Matthew at handoff)

1. **Step arithmetic is `:bump`'s, not `nudge_text`'s.** A block step adds `steps × 10^-precision` (or `steps` on an `I64` column) to each cell's exact current value through `draft::bumped`, without snapping to the painted grid. The single-cell editor nudge (text, snapped) is unchanged. Snapping a block would silently rewrite the unpainted decimals of every stepped cell.
2. **The market-data footer shows the extent only** (`3 rows × 4 cols`), while a selection is live. Vol and forward ladders do not add up, and the blotter footer was already trimmed on request. Totals can be added later on request.
3. **`i` with a selection opens on the cursor cell and refuses when that cell refuses** (a deleted row, a document with nothing to edit), exactly as today. It does not hunt for another member.
4. **A key switch clears the selection.** The next underlying's terms can carry the same labels, and a selection must never carry over to another document.
5. **An escape after a rebase keeps the steps.** If the painted base changed while the bulk editor was open (auto rebase or replace), `escape` does not restore the pre-edit draft. It closes the editor and says `steps kept: the document moved`.

## Review Focus

1. **A delivery lands while the bulk editor is open (Hold policy → `Behind`).** `escape` must restore the edits and keep `Behind`, never hide it. This is pinned in Task 7 (`escape_after_steps_keeps_a_behind_that_arrived_meanwhile`).
2. **An inserted row inside the selection.** Steps, commits and `:bump` must write its cells through `set_row_cell` by label, never `Draft::set` by position. This is pinned in Task 5 (`a_selection_bump_reaches_an_inserted_rows_cells`). Tasks 6 and 7 share `write_steps` and `commit_bulk`'s identical `RowState::Inserted` arm.
3. **A key switch with a live selection.** It must clear rather than re-resolve onto the same term labels in another document. This is pinned in Task 2 (`a_key_switch_clears_the_selection`).
4. **A fractional step on a mixed block (an `I64` column beside `F64`).** It must write nothing at all. This is pinned in Task 5 (`a_fractional_bump_over_a_mixed_block_writes_nothing`).
5. **Typing after stepping, then `enter`.** The typed value must replace the steps on every accepting cell. A cell that refuses the typed value must go back to its pre-`i` value, not keep a half-step. This is pinned in Task 7 (`typing_after_steps_replaces_them_and_a_refusing_cell_returns_to_before`).

---

## File Structure

- Create `crates/geode-marketdata/src/core/bulk.rs`: pure rules for per-kind acceptance of a typed value (`accept`), step size (`step_delta`), and skip counting and notices (`Skip`, `Skips`, `set_notice`, `step_notice`).
- Modify `crates/geode-marketdata/src/core/mod.rs`: `pub mod bulk;`.
- Modify `crates/geode-marketdata/src/core/cursor.rs`: `step_clamped`.
- Modify `crates/geode-marketdata/src/core/draft.rs`: `Draft::restore_from`.
- Create `crates/geode-marketdata/src/tile/select.rs`: the tile's selection state doors and every selection verb.
- Create `crates/geode-marketdata/src/tile/tests/selection.rs`: tile tests over the existing `Harness`.
- Modify `crates/geode-marketdata/src/tile.rs`:
  - fields `selection`, `resolved`, `selection_extent`;
  - `key_context`;
  - `dispatch` arms;
  - `sync_cursor` (becomes `&mut self` and refreshes);
  - `commit_edit`, `commit_cell_edit`, `pick_option` and `nudge` hooks;
  - `close_editor` restore;
  - `Editing.bulk`;
  - `set_key` clear;
  - `render` footer;
  - `mod select;`, plus `mod selection;` inside `mod tests`.
- Modify `crates/geode-marketdata/src/delegate.rs`:
  - `selected: Option<Resolved>` mirror and tint;
  - `CellPointer` emission (Task 8).
- Modify `crates/geode-marketdata/src/content.rs`: normal `v`/`shift+v`; the `mode == visual` block; action registry rows.
- Modify `crates/geode-marketdata/src/commands.rs`: `Command::Bump { axis: Option<BumpAxis> }`.
- Modify `crates/geode-marketdata/README.md`, `docs/current/features.md` and `docs/current/keymaps.md` (Task 9).
- Modify `scripts/mutation-check.sh`: five entries (Task 9).

Test fixture facts used below (`tile.rs` tests):
- `open(cx)` + `h.with_document(&mut vcx)` paints the CVI fixture.
  - Two rows, terms `row 0` / `row 1`.
  - Model columns 0–2 are the slice values: fwd `4500.00`/`4510.00` (2 places), atm `0.1800`/`0.1900`, skew `-1.0000`/`-1.1000`.
  - Columns 3–5 are the nodes: row 0 `0.1000 0.2000 0.3000`, row 1 `0.4000 0.5000 0.6000` (4 places).
  - `const SLICE: usize = 3`. The cursor starts at `Cell{0,0}`.
- `open_flat(cx)` + `h.with_flat_document(&mut vcx)` paints `SCHEDULE`.
  - Rows `D1`/`D2`.
  - Columns: a Date (`2026-12-18`), a Number `amount` (`1.25`/`0.5`), a Choice status (`declared`/`estimated`).
  - Read the fixture (`core/test_fixtures.rs`) for exact column order before writing flat assertions.
- Helpers:
  - `h.dispatch(vcx, verb, count)`, `h.command(vcx, line)`, `h.mode(vcx)`;
  - `h.cell(vcx,r,c) -> (text, edited)`, `h.row_texts`, `h.col_texts`;
  - `h.editor_value`, `h.set_editor(vcx, text)` (a programmatic `set_value`, which emits no `Change`), `h.header_texts`;
  - `clipboard(vcx)`, `centre_of(vcx, selector)`, `click_at(vcx, at, count)`;
  - `h.tile.read_with(vcx, |t, _| t.draft().len())`.

---

### Task 1: Pure rules — acceptance, step size, notices, clamped motion, draft restore

**Files:**
- Create: `crates/geode-marketdata/src/core/bulk.rs`
- Modify: `crates/geode-marketdata/src/core/mod.rs`, `crates/geode-marketdata/src/core/cursor.rs`, `crates/geode-marketdata/src/core/draft.rs`

**Interfaces:**
- Produces:
  - `bulk::Skip` (`Deleted`, `Empty`, `NotNumeric`, `WrongType`, `Required`, `NotAnOption`; `Ord`);
  - `bulk::Skips { add(Skip), total() -> usize, describe() -> String }`;
  - `bulk::accept(kind: &CellKind, ty: Option<ColumnType>, required: bool, text: &str) -> Result<Value, Skip>`;
  - `bulk::step_delta(ty: ColumnType, precision: u8, steps: i64) -> f64`;
  - `bulk::set_notice(n: usize, skips: &Skips) -> String`;
  - `bulk::step_notice(n: usize, total_steps: i64, skips: &Skips) -> String`;
  - `cursor::step_clamped(cursor: Cursor, motion: Motion, grid: Grid) -> Cursor`;
  - `Draft::restore_from(&mut self, before: Draft)`.

- [ ] **Step 1: Write the failing tests** (bottom of `bulk.rs`, `cursor.rs` tests, `draft.rs` tests)

```rust
// core/bulk.rs
#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::view::ColumnFormat;

    #[test]
    fn a_number_cell_parses_by_its_declared_type_and_refuses_otherwise() {
        let n = CellKind::Number(ColumnFormat::default());
        assert_eq!(accept(&n, Some(ColumnType::F64), false, " 0.25 "), Ok(Value::F64(0.25)));
        assert_eq!(accept(&n, Some(ColumnType::I64), false, "3"), Ok(Value::I64(3)));
        assert_eq!(accept(&n, Some(ColumnType::I64), false, "0.5"), Err(Skip::WrongType));
        assert_eq!(accept(&n, Some(ColumnType::F64), false, "abc"), Err(Skip::WrongType));
    }

    #[test]
    fn text_choice_and_date_cells_each_keep_their_own_rule() {
        assert_eq!(accept(&CellKind::Text, None, false, " x "), Ok(Value::Utf8("x".into())));
        assert_eq!(accept(&CellKind::Text, None, true, "  "), Err(Skip::Required));
        let c = CellKind::Choice(&["declared", "estimated"]);
        assert_eq!(accept(&c, None, false, "declared"), Ok(Value::Utf8("declared".into())));
        assert_eq!(accept(&c, None, false, "maybe"), Err(Skip::NotAnOption));
        assert!(matches!(accept(&CellKind::Date, None, false, "2027-03-19"), Ok(Value::Date(_))));
        assert_eq!(accept(&CellKind::Date, None, false, "0.25"), Err(Skip::WrongType));
    }

    #[test]
    fn a_step_is_one_unit_of_the_columns_places_or_one_on_an_integer() {
        assert_eq!(step_delta(ColumnType::F64, 4, 1), 0.0001);
        assert_eq!(step_delta(ColumnType::F64, 2, -10), -0.1);
        assert_eq!(step_delta(ColumnType::I64, 0, 10), 10.0);
    }

    #[test]
    fn notices_count_cells_and_name_each_skip_reason_in_a_fixed_order() {
        let mut s = Skips::default();
        assert_eq!(set_notice(1, &s), "set 1 cell");
        s.add(Skip::WrongType);
        s.add(Skip::Deleted);
        s.add(Skip::Deleted);
        assert_eq!(set_notice(12, &s), "set 12 cells, skipped 3 (2 deleted, 1 wrong type)");
        assert_eq!(step_notice(9, 12, &Skips::default()), "stepped 9 cells +12");
        assert_eq!(step_notice(2, -1, &s), "stepped 2 cells -1, skipped 3 (2 deleted, 1 wrong type)");
    }
}
```

```rust
// core/cursor.rs, inside mod tests
#[test]
fn a_clamped_step_never_wraps_and_never_enters_the_strip() {
    assert_eq!(step_clamped(cell(0, 1), Motion::Rows(-1), G), cell(0, 1));
    assert_eq!(step_clamped(cell(4, 1), Motion::Rows(1), G), cell(4, 1));
    assert_eq!(step_clamped(cell(1, 1), Motion::Rows(10), G), cell(4, 1));
    assert_eq!(step_clamped(cell(1, 3), Motion::Cols(1), G), cell(1, 3));
    assert_eq!(step_clamped(cell(1, 2), Motion::Top, G), cell(0, 2));
    assert_eq!(step_clamped(cell(1, 2), Motion::LastCol, G), cell(1, 3));
    // An attribute cursor has no selection to extend; it stays put.
    assert_eq!(step_clamped(Cursor::Attr(1), Motion::Rows(1), G), Cursor::Attr(1));
}
```

```rust
// core/draft.rs, inside mod tests — use the module's existing base/labels helpers
#[test]
fn restore_from_puts_back_the_edits_and_keeps_a_behind_reached_meanwhile() {
    let base = DocumentBase::default();
    let mut before = Draft::default();
    before.set((0, 0), ("a".into(), "x".into()), Value::F64(1.0), &base);
    let mut now = before.clone();
    now.set((0, 1), ("a".into(), "y".into()), Value::F64(2.0), &base);
    let newer = DocumentBase { as_of: "2026-09-27T10:00:00Z".into(), generation: Some(8) };
    now.on_delivered(&newer);
    assert!(now.is_behind());

    now.restore_from(before.clone());
    assert_eq!(now.len(), 1, "the step's edit is gone");
    assert!(now.is_behind(), "the delivery is still news");

    // Restoring to an empty draft is Clean: an empty draft is never behind.
    now.restore_from(Draft::default());
    assert!(now.is_empty() && !now.is_behind());
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p geode-marketdata --lib -- bulk:: a_clamped_step restore_from`
Expected: compile errors (`accept`, `step_clamped`, `restore_from` not found).

- [ ] **Step 3: Implement**

```rust
// core/bulk.rs
//! Rules a selection-wide edit applies per cell (grid selection spec
//! §4.4): whether a typed value lands in a cell of a given kind, how
//! far one arrow step moves a numeric cell, and the one notice line
//! that counts what was written and what was skipped and why.

use crate::core::draft::{parse_attr, parse_cell};
use crate::core::matrix::CellKind;
use geode_core::document::Value;
use geode_core::schema::ColumnType;
use std::collections::BTreeMap;

/// Why a selected cell took no part in a bulk edit. The order is the
/// notice's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Skip {
    Deleted,
    Empty,
    NotNumeric,
    WrongType,
    Required,
    NotAnOption,
}

impl Skip {
    fn phrase(self) -> &'static str {
        match self {
            Skip::Deleted => "deleted",
            Skip::Empty => "empty",
            Skip::NotNumeric => "not numeric",
            Skip::WrongType => "wrong type",
            Skip::Required => "required",
            Skip::NotAnOption => "not an option",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Skips(BTreeMap<Skip, usize>);

impl Skips {
    pub fn add(&mut self, skip: Skip) {
        *self.0.entry(skip).or_default() += 1;
    }

    pub fn total(&self) -> usize {
        self.0.values().sum()
    }

    /// `""` when nothing was skipped, else `", skipped 3 (2 deleted, 1 wrong type)"`.
    pub fn describe(&self) -> String {
        if self.0.is_empty() {
            return String::new();
        }
        let parts: Vec<String> = self.0.iter().map(|(s, n)| format!("{n} {}", s.phrase())).collect();
        format!(", skipped {} ({})", self.total(), parts.join(", "))
    }
}

fn cells(n: usize) -> String {
    format!("{n} cell{}", if n == 1 { "" } else { "s" })
}

pub fn set_notice(n: usize, skips: &Skips) -> String {
    format!("set {}{}", cells(n), skips.describe())
}

/// `total_steps` is the signed count since the editor opened, so the line
/// says where the block stands, not only the last press.
pub fn step_notice(n: usize, total_steps: i64, skips: &Skips) -> String {
    format!("stepped {} {total_steps:+}{}", cells(n), skips.describe())
}

/// Whether `text` lands in a cell of `kind`, and as what. `ty` is the
/// column's declared type for a `Number` cell (`None` for other kinds).
/// A choice must name one of its options exactly (after trimming): a bulk
/// write has no popup to rank a near miss against.
pub fn accept(kind: &CellKind, ty: Option<ColumnType>, required: bool, text: &str) -> Result<Value, Skip> {
    match kind {
        CellKind::Number(_) => {
            let ty = ty.ok_or(Skip::WrongType)?;
            parse_cell(text, ty).map_err(|_| Skip::WrongType)
        }
        CellKind::Text => {
            let t = text.trim();
            if t.is_empty() && required {
                Err(Skip::Required)
            } else {
                Ok(Value::Utf8(t.to_string()))
            }
        }
        CellKind::Choice(options) => {
            let t = text.trim();
            options
                .iter()
                .find(|o| **o == t)
                .map(|o| Value::Utf8((*o).to_string()))
                .ok_or(Skip::NotAnOption)
        }
        CellKind::Date => parse_attr(text, ColumnType::Date).map_err(|_| Skip::WrongType),
    }
}

/// One arrow step on a numeric column: one unit of its painted places, or
/// a whole one on an integer column. The value it is added to is exact
/// (`draft::bumped`), never snapped to the painted grid.
pub fn step_delta(ty: ColumnType, precision: u8, steps: i64) -> f64 {
    match ty {
        ColumnType::I64 => steps as f64,
        _ => steps as f64 / 10f64.powi(i32::from(precision)),
    }
}
```

```rust
// core/cursor.rs
/// One motion while a selection is live (grid selection spec §4.1):
/// every move clamps at the grid's edges instead of wrapping, and none
/// leaves the grid for the attribute strip, because wrapping past the
/// anchor would silently invert the selection.
pub fn step_clamped(cursor: Cursor, motion: Motion, grid: Grid) -> Cursor {
    use geode_shell::vimnav::{NavCommand, apply_clamped};
    let Cursor::Cell { row, col } = cursor else {
        return cursor;
    };
    if grid.rows == 0 || grid.cols == 0 {
        return Cursor::Cell { row: 0, col: 0 };
    }
    let max_col = grid.cols - 1;
    match motion {
        Motion::Rows(n) => Cursor::Cell { row: apply_clamped(row, grid.rows, NavCommand::Move(n as i64)), col },
        Motion::Cols(n) => Cursor::Cell { row, col: add(col, n, max_col) },
        Motion::Top => Cursor::Cell { row: 0, col },
        Motion::Bottom => Cursor::Cell { row: grid.rows - 1, col },
        Motion::FirstCol => Cursor::Cell { row, col: 0 },
        Motion::LastCol => Cursor::Cell { row, col: max_col },
    }
}
```

```rust
// core/draft.rs, impl Draft
/// Put back `before` — the open bulk editor's undo (grid selection spec
/// §4.4). A `Behind` this draft reached since is kept: a delivery that
/// landed while the editor was open is still news, and restoring the
/// older state would hide it. An empty result is `Clean`, since an empty
/// draft is never behind.
pub fn restore_from(&mut self, before: Draft) {
    let behind = matches!(self.state, DraftState::Behind { .. }).then(|| self.state.clone());
    *self = before;
    match behind {
        Some(state) if !self.is_empty() => self.state = state,
        _ if self.is_empty() => {
            self.state = DraftState::Clean;
            self.base = None;
        }
        _ => {}
    }
}
```

Add `pub mod bulk;` to `core/mod.rs` (alphabetical, before `cursor`).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p geode-marketdata --lib -- bulk:: a_clamped_step restore_from`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-marketdata/src/core
git commit -m "feat(marketdata): pure rules for selection-wide edits

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: Selection state, keys, tint, footer extent, lost anchor

**Files:**
- Create: `crates/geode-marketdata/src/tile/select.rs`, `crates/geode-marketdata/src/tile/tests/selection.rs`
- Modify: `crates/geode-marketdata/src/tile.rs`, `crates/geode-marketdata/src/delegate.rs`, `crates/geode-marketdata/src/content.rs`

**Interfaces:**
- Consumes: `geode_core::grid::selection::{Selection, SelectKind, Resolved, Lost}`, `cursor::step_clamped`.
- Produces (on `MarketDataTile`, `pub(super)` or private within `tile`):
  - fields `selection: Option<Selection<SharedString, SharedString>>`, `resolved: Option<Resolved>`, `selection_extent: Option<SharedString>`;
  - `fn start_selection(&mut self, kind: SelectKind)` and `fn clear_selection(&mut self)`;
  - `fn refresh_selection(&mut self) -> bool` (true when the anchor was lost and the notice was set);
  - `fn selection_cells(&self) -> Vec<(usize, usize)>` (resolved members, `Rows` skipping `slice_columns`, row-label column never included);
  - `pub(crate) fn resolved(&self) -> Option<&Resolved>` (test reader);
  - delegate field `pub(crate) selected: Option<Resolved>`.

- [ ] **Step 1: Write the failing tests** in `src/tile/tests/selection.rs` (declare `mod selection;` as the last item inside `tile.rs`'s `mod tests`; start the file with `use super::*;`)

```rust
use super::*;
use geode_core::grid::selection::SelectKind;

fn resolved(h: &Harness, vcx: &gpui::VisualTestContext) -> Option<(SelectKind, std::ops::Range<usize>, std::ops::Range<usize>)> {
    h.tile.read_with(vcx, |t, _| t.resolved().map(|r| (r.kind, r.rows.clone(), r.cols.clone())))
}

#[gpui::test]
fn v_starts_a_block_and_motions_extend_it_clamped(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "right", Some(SLICE as u32));
    h.dispatch(&mut vcx, "visual_block", None);
    assert_eq!(h.mode(&vcx), "visual");
    h.dispatch(&mut vcx, "right", None);
    h.dispatch(&mut vcx, "down", Some(5)); // clamps at the last row, no wrap
    assert_eq!(resolved(&h, &vcx), Some((SelectKind::Block, 0..2, 3..5)));
    h.dispatch(&mut vcx, "up", Some(9)); // clamps at row 0, never the strip
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.cursor()), Cursor::Cell { row: 0, col: 4 });
}

#[gpui::test]
fn shift_v_switches_kind_keeping_the_anchor_and_the_same_key_clears(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "visual_block", None);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "visual_rows", None);
    assert_eq!(resolved(&h, &vcx), Some((SelectKind::Rows, 0..2, 0..6)), "every column, anchor kept");
    h.dispatch(&mut vcx, "visual_rows", None);
    assert_eq!(resolved(&h, &vcx), None);
    assert_eq!(h.mode(&vcx), "normal");
}

#[gpui::test]
fn escape_clears_only_the_selection_first(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "step", None); // fwd is not a choice cell: leaves a notice
    let notice = || "not a choice cell".to_string();
    assert!(h.header_texts(&vcx).contains(&notice()));
    h.dispatch(&mut vcx, "visual_block", None);
    h.dispatch(&mut vcx, "escape", None);
    assert_eq!(resolved(&h, &vcx), None);
    assert!(h.header_texts(&vcx).contains(&notice()), "the first escape cleared only the selection");
    h.dispatch(&mut vcx, "escape", None);
    assert!(!h.header_texts(&vcx).contains(&notice()), "the second does today's escape");
}

#[gpui::test]
fn v_in_the_attribute_strip_is_refused(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "up", None); // row 0 → the strip
    h.dispatch(&mut vcx, "visual_block", None);
    assert_eq!(resolved(&h, &vcx), None);
    assert!(h.header_texts(&vcx).iter().any(|t| t == "select from a grid cell"));
}

#[gpui::test]
fn a_key_switch_clears_the_selection(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.command(&mut vcx, "key SPX.Y").unwrap();
    assert_eq!(resolved(&h, &vcx), None);
    assert_eq!(h.mode(&vcx), "normal");
}

#[gpui::test]
fn an_anchor_row_that_disappears_clears_the_selection_with_a_notice(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    let tag = h.with_document_tagged(&mut vcx);
    h.dispatch(&mut vcx, "down", None); // anchor on row 1
    h.dispatch(&mut vcx, "visual_rows", None);
    // A newer one-term generation: the anchor's term (TERMS[1]) is gone. The
    // draft is clean, so it simply paints.
    h.deliver(&mut vcx, tag, Arc::new(document_of(&TERMS[..1], &NODES, NEWER)));
    assert_eq!(resolved(&h, &vcx), None);
    assert!(h.header_texts(&vcx).iter().any(|t| t == "selection cleared: anchor row no longer shown"));
}

#[gpui::test]
fn the_block_tints_its_cells_and_the_footer_shows_the_extent(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "right", Some(SLICE as u32));
    h.dispatch(&mut vcx, "visual_block", None);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "right", None);
    let selected = h.tile.read_with(&vcx, |t, cx| t.table.read(cx).delegate().selected.clone());
    assert_eq!(selected.map(|r| (r.rows, r.cols)), Some((0..2, 3..5)));
    draw(&mut vcx);
    assert!(vcx.debug_bounds("aggregate-extent").is_some(), "the footer strip is painted");
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.selection_extent.clone()).as_deref(), Some("2 rows × 2 cols"));
}
```

(`TERMS`, `NODES`, `NEWER` and `document_of` are the `tile.rs` test module's own. The same tag redelivers, as `edit_and_bump_are_refused_while_behind` does.)

Add to `content.rs` tests, beside `dot_and_u_bind_in_normal_mode_and_the_menu_keys_in_menu_mode`:

```rust
#[test]
fn v_and_shift_v_start_the_two_selections_and_visual_binds_single_key_verbs() {
    let doc = fragment_doc(CVI.kind, DEFAULT_KEYMAP).unwrap();
    let (keymap, diags) = build_keymap(&[doc], default_mod(), &registry());
    assert!(diags.is_empty(), "{diags:?}");
    let stack_for = |mode: &str| {
        [
            KeyContext::new("workspace"),
            KeyContext::new("tile"),
            KeyContext::new("marketdata").pair("mode", mode).counts(),
        ]
    };
    let (normal, visual) = (stack_for("normal"), stack_for("visual"));
    for (stack, spec, expected) in [
        (&normal, "v", "marketdata::visual_block"),
        (&normal, "shift+v", "marketdata::visual_rows"),
        (&visual, "v", "marketdata::visual_block"),
        (&visual, "shift+v", "marketdata::visual_rows"),
        (&visual, "j", "marketdata::down"),
        (&visual, "y", "marketdata::yank"),
        (&visual, "d", "marketdata::delete_row"),
        (&visual, "i", "marketdata::edit"),
        (&visual, "escape", "marketdata::escape"),
    ] {
        let keystroke = parse_keystroke(spec, default_mod()).unwrap();
        match Matcher::default().press(&keymap, keystroke, stack) {
            MatchResult::Matched { action, .. } => assert_eq!(action.0, expected, "{spec}"),
            other => panic!("{spec}: expected a match, got {other:?}"),
        }
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p geode-marketdata --lib -- selection:: v_and_shift_v`
Expected: compile errors (`resolved()`, `visual_block` unhandled) or FAIL.

- [ ] **Step 3: Implement**

`content.rs`:
- In the `mode == normal` block add `"v" = "marketdata::visual_block"` and `"shift+v" = "marketdata::visual_rows"`.
- Add a block:

```toml
[[bindings]]
context = "marketdata && mode == visual"
[bindings.keys]
"j" = "marketdata::down"
"k" = "marketdata::up"
"h" = "marketdata::left"
"l" = "marketdata::right"
"g g" = "marketdata::top"
"shift+g" = "marketdata::bottom"
"^" = "marketdata::first_col"
"$" = "marketdata::last_col"
"home" = "marketdata::first_col"
"end" = "marketdata::last_col"
"ctrl+d" = "marketdata::page_down"
"ctrl+u" = "marketdata::page_up"
"ctrl+f" = "marketdata::page_down_full"
"ctrl+b" = "marketdata::page_up_full"
"pagedown" = "marketdata::page_down_full"
"pageup" = "marketdata::page_up_full"
"y" = "marketdata::yank"
"d" = "marketdata::delete_row"
"i" = "marketdata::edit"
"enter" = "marketdata::edit"
"v" = "marketdata::visual_block"
"shift+v" = "marketdata::visual_rows"
"escape" = "marketdata::escape"
```

- Update the `DEFAULT_KEYMAP` doc comment to name the `visual` context.
- Add `("marketdata::visual_rows", "Select rows")` and `("marketdata::visual_block", "Select cells")` to the action registry list, beside `delete_row`.

`tile.rs`:
- Add `mod select;` near the other `mod`/`use` lines.
- Add the three fields to `MarketDataTile` (doc comments as in the blotter's `delegate.rs:90–114`) and initialise them to `None` in `new`.
- `key_context`: after the insert/menu checks, report `"visual"` when `self.selection.is_some()`, and add `.pair("select", "rows"|"block")` from the kind (the blotter's `tile.rs:962`).
- `sync_cursor(&self, …)` becomes `sync_cursor(&mut self, …)`. Its first lines are:

```rust
if self.refresh_selection() {
    self.rebuild_chrome();
}
```

  and then the mirror adds `d.selected = self.resolved.clone();` in both arms (`None` in the `Attr` arm). Fix every caller that held `&self`; they are all `&mut self` or closures over `this`.
- `dispatch` motion arm:

```rust
self.cursor = if self.selection.is_some() {
    cursor::step_clamped(self.cursor, motion, grid)
} else {
    cursor::step(self.cursor, &mut self.last_grid_col, motion, grid)
};
```

- New arms before `"escape"`:

```rust
"visual_rows" | "visual_block" => {
    let kind = if verb == "visual_rows" { SelectKind::Rows } else { SelectKind::Block };
    self.start_selection(kind);
    true
}
```

- In `"escape"`, first:

```rust
if self.selection.is_some() {
    self.clear_selection();
    return { self.sync_cursor(cx); cx.notify(); true };
}
```

  Spell it as a block that clears, syncs, notifies and returns `true`, matching the arm's style.
- `set_key`: call `self.clear_selection()` before the key changes.

`tile/select.rs`:

```rust
//! The panel's grid selection (grid selection spec §4.1, §4.4): state
//! doors, and every verb that takes the selection as its operand.
//! Anchored by row label and column label, so a redelivery, an inserted
//! row or a rebase keeps it on the same cells; an anchor no longer
//! painted clears it with a notice rather than guessing a neighbour.

use super::*;
use geode_core::grid::selection::{Lost, SelectKind, Selection};

impl MarketDataTile {
    /// `v`/`V`: start at the cursor cell, switch kind keeping the anchor,
    /// or clear on the same kind again. Refused in the attribute strip,
    /// which is never a member.
    pub(super) fn start_selection(&mut self, kind: SelectKind) {
        match self.selection.as_ref().map(|s| s.kind) {
            Some(k) if k == kind => self.selection = None,
            Some(_) => {
                if let Some(s) = self.selection.as_mut() {
                    s.kind = kind;
                }
            }
            None => {
                let Cursor::Cell { row, col } = self.cursor else {
                    self.notice = Some("select from a grid cell".into());
                    return;
                };
                let (Some(r), Some(c)) = (self.model.rows.get(row), self.model.columns.get(col)) else {
                    return;
                };
                self.selection = Some(Selection { kind, anchor_row: r.label.clone(), anchor_col: c.clone() });
            }
        }
    }

    pub(super) fn clear_selection(&mut self) {
        self.selection = None;
        self.resolved = None;
        self.selection_extent = None;
    }

    /// Re-resolve against the current model and cursor, preparing the
    /// footer extent. Answers whether the anchor was lost (the selection
    /// is then cleared and the notice set), so the caller re-prepares
    /// the header.
    pub(super) fn refresh_selection(&mut self) -> bool {
        let Some(sel) = &self.selection else {
            self.resolved = None;
            self.selection_extent = None;
            return false;
        };
        let outcome = match self.cursor {
            Cursor::Cell { row, col } => sel.resolve_with(
                (row, col),
                self.model.columns.len(),
                |label| self.model.rows.iter().position(|r| &r.label == label),
                |name| self.model.columns.iter().position(|c| c == name),
            ),
            // Unreachable: motions clamp out of the strip and `v` refuses there.
            Cursor::Attr(_) => Err(Lost::Row),
        };
        match outcome {
            Ok(r) => {
                let (rows, cols) = (r.rows.len(), r.cols.len());
                let plural = |n: usize| if n == 1 { "" } else { "s" };
                self.selection_extent = Some(format!("{rows} row{} × {cols} col{}", plural(rows), plural(cols)).into());
                self.resolved = Some(r);
                false
            }
            Err(lost) => {
                self.clear_selection();
                self.notice = Some(match lost {
                    Lost::Row => "selection cleared: anchor row no longer shown",
                    Lost::Column => "selection cleared: anchor column no longer shown",
                }.into());
                true
            }
        }
    }

    /// The model cells a selection-wide edit visits, row-major. A `Rows`
    /// selection skips the leading slice values (`:bump row`'s rule: a
    /// term's forward/atm/skew never move with its ladder); a `Block` is
    /// exactly its rectangle. The row-label column is never a model
    /// column, so it is never here.
    pub(super) fn selection_cells(&self) -> Vec<(usize, usize)> {
        let Some(r) = &self.resolved else { return Vec::new() };
        let skip = match r.kind {
            SelectKind::Rows => self.model.slice_columns,
            SelectKind::Block => 0,
        };
        r.rows
            .clone()
            .flat_map(|row| r.cols.clone().filter(move |&c| c >= skip).map(move |c| (row, c)))
            .collect()
    }

    pub(crate) fn resolved(&self) -> Option<&Resolved> {
        self.resolved.as_ref()
    }
}
```

(Pull `Resolved` and `SelectKind` into `tile.rs`'s `use` list as needed; `select.rs` uses `super::*`.)

`delegate.rs`:
- Add `pub(crate) selected: Option<Resolved>` (doc: "The tile's resolved selection, mirrored by `sync_cursor`; `render_cell` only looks it up"), initialised `None`.
- In `render_cell`'s value arm compute `let in_selection = self.selected.as_ref().is_some_and(|r| r.contains(row_ix, model_col));`.
- In the label arm compute `self.selected.as_ref().is_some_and(|r| r.kind == SelectKind::Rows && r.contains_row(row_ix))`.
- The tint must not hide an edited cell's own fill, so paint it as the cell's first child, an absolute overlay under the text:

```rust
.relative()
.when(in_selection, |el| {
    el.child(div().absolute().inset_0().bg(theme.selection.opacity(0.35)))
})
```

  Apply it before `.child(text/editor)`, and keep the cursor border after it.

`tile.rs` `render`: append a footer after `body` only while a selection is live.

```rust
.when_some(self.selection_extent.as_ref(), |el, extent| {
    el.child(
        h_flex()
            .w_full()
            .h(scale::design(FOOTER_HEIGHT))
            .items_center()
            .px_2()
            .text_xs()
            .border_t_1()
            .border_color(theme.border)
            .child(aggregates::strip(Some(extent), &[], &[], theme)),
    )
})
```

Add `const FOOTER_HEIGHT: f32 = 20.0;` beside `HALF_PAGE` (the blotter's, pricer's and timeseries' value) with a one-line doc. Import `geode_shell::shell::aggregates`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p geode-marketdata`
Expected: PASS (every existing test too; `sync_cursor` becoming `&mut` must not change behaviour).

- [ ] **Step 5: Commit**

```bash
git add crates/geode-marketdata/src
git commit -m "feat(marketdata): V and v select rows and cell blocks

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: `y` copies the selection as TSV

**Files:**
- Modify: `crates/geode-marketdata/src/tile/select.rs`, `crates/geode-marketdata/src/tile.rs` (`"yank"` arm)
- Test: `crates/geode-marketdata/src/tile/tests/selection.rs`

**Interfaces:**
- Consumes: `resolved`, `clear_selection`.
- Produces: `fn selection_tsv(&self) -> Option<String>`.

- [ ] **Step 1: Write the failing tests**

```rust
#[gpui::test]
fn y_over_rows_copies_a_header_and_every_painted_column_then_ends_the_selection(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "yank", None);
    let text = clipboard(&mut vcx).expect("copied");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "a header and two rows");
    assert!(lines[1].ends_with("4500.00\t0.1800\t-1.0000\t0.1000\t0.2000\t0.3000"));
    assert!(lines[2].ends_with("4510.00\t0.1900\t-1.1000\t0.4000\t0.5000\t0.6000"));
    assert_eq!(h.mode(&vcx), "normal", "y consumes the selection");
}

#[gpui::test]
fn y_over_a_block_copies_its_columns_header_and_cells_only(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "right", Some(SLICE as u32 + 1));
    h.dispatch(&mut vcx, "visual_block", None);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "right", None);
    h.dispatch(&mut vcx, "yank", None);
    let columns = h.tile.read_with(&vcx, |t, _| t.model().columns[4..6].join("\t"));
    assert_eq!(clipboard(&mut vcx).as_deref(), Some(format!("{columns}\n0.2000\t0.3000\n0.5000\t0.6000").as_str()));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p geode-marketdata --lib -- selection::y_`
Expected: FAIL (the `"yank"` arm copies the cursor cell).

- [ ] **Step 3: Implement**

```rust
// tile/select.rs
/// `y` in visual mode (spec §4.2): tab-separated, a header line first.
/// `Rows` copies what `y y` copies for each row (the label where it is
/// painted, then every column) under a header of the same shape;
/// `Block` copies its own columns' header and cells, no label.
pub(super) fn selection_tsv(&self) -> Option<String> {
    let r = self.resolved.as_ref()?;
    let label = matches!(r.kind, SelectKind::Rows) && self.spec.rows.shown();
    let mut out = Vec::with_capacity(r.rows.len() + 1);
    let header: Vec<&str> = label
        .then_some(self.spec.rows.column)
        .into_iter()
        .chain(r.cols.clone().filter_map(|c| self.model.columns.get(c).map(|s| s.as_ref())))
        .collect();
    out.push(header.join("\t"));
    for row in r.rows.clone() {
        let m = self.model.rows.get(row)?;
        let line: Vec<&str> = label
            .then(|| m.label.as_ref())
            .into_iter()
            .chain(r.cols.clone().filter_map(|c| m.cells.get(c).map(|cell| cell.text.as_ref())))
            .collect();
        out.push(line.join("\t"));
    }
    Some(out.join("\n"))
}
```

In the `"yank"` arm, first:

```rust
if self.selection.is_some() {
    if let Some(text) = self.selection_tsv() {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }
    self.clear_selection();
    false
} else { /* today's body */ }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p geode-marketdata --lib -- selection:: yank`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git commit -am "feat(marketdata): y copies a selection as TSV

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: `d` deletes selected rows; a block refuses

**Files:**
- Modify: `crates/geode-marketdata/src/tile/select.rs`, `crates/geode-marketdata/src/tile.rs` (`"delete_row"` arm)
- Test: `crates/geode-marketdata/src/tile/tests/selection.rs`

**Interfaces:**
- Produces: `fn delete_selected_rows(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String>`.

- [ ] **Step 1: Write the failing tests**

```rust
#[gpui::test]
fn d_over_rows_deletes_them_in_one_draft_change_and_ends_the_selection(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "delete_row", None);
    let states = h.tile.read_with(&vcx, |t, _| t.model().rows.iter().map(|r| r.state).collect::<Vec<_>>());
    assert_eq!(states, vec![RowState::Deleted, RowState::Deleted]);
    assert_eq!(h.mode(&vcx), "normal");
    h.command(&mut vcx, "revert").unwrap();
    let states = h.tile.read_with(&vcx, |t, _| t.model().rows.iter().map(|r| r.state).collect::<Vec<_>>());
    assert_eq!(states, vec![RowState::Document, RowState::Document], "one revert restores both");
}

#[gpui::test]
fn d_over_a_block_refuses_and_names_v(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "visual_block", None);
    h.dispatch(&mut vcx, "delete_row", None);
    assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_empty()));
    assert!(h.header_texts(&vcx).iter().any(|t| t == "d deletes rows — use V"));
    assert_eq!(h.mode(&vcx), "visual", "a refusal keeps the selection");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p geode-marketdata --lib -- selection::d_over`
Expected: FAIL.

- [ ] **Step 3: Implement**

```rust
// tile/select.rs
/// `d` in visual mode (spec §4.4, §4.6): a `Rows` selection deletes
/// every selected row — labels collected first, because dropping an
/// inserted row shifts every later index — and ends the selection; a
/// `Block` refuses rather than acting on the rows it happens to touch.
pub(super) fn delete_selected_rows(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
    if self.resolved.as_ref().is_some_and(|r| r.kind == SelectKind::Block) {
        return Err("d deletes rows — use V".to_string());
    }
    let (_, base) = self.row_verb_target(window, cx)?;
    let labels: Vec<String> = self
        .resolved
        .as_ref()
        .map(|r| r.rows.clone().filter_map(|i| self.model.rows.get(i).map(|m| m.label.to_string())).collect())
        .unwrap_or_default();
    let mut already = 0usize;
    for label in &labels {
        if self.draft.delete_row(label, &base) == RowDelete::Already {
            already += 1;
        }
    }
    if already == labels.len() {
        return Err(ALREADY_DELETED.to_string());
    }
    self.clear_selection();
    self.notice = None;
    self.rebuild_model(cx);
    Ok(())
}
```

In `dispatch`'s `"delete_row"` arm, call `delete_selected_rows` when `self.selection.is_some()`, else today's `delete_row`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p geode-marketdata`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git commit -am "feat(marketdata): d deletes selected rows; a block refuses

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: One step writer; `:bump` with no axis acts on the selection

**Files:**
- Modify: `crates/geode-marketdata/src/commands.rs`, `crates/geode-marketdata/src/tile.rs` (`bump`, `command`), `crates/geode-marketdata/src/tile/select.rs`
- Test: `crates/geode-marketdata/src/commands.rs` tests, `crates/geode-marketdata/src/tile/tests/selection.rs`

**Interfaces:**
- Consumes: `bulk::{Skip, Skips, step_delta}`.
- Produces:
  - `Command::Bump { delta: f64, axis: Option<BumpAxis> }`;
  - `fn current_numeric(&self, row: usize, col: usize) -> Option<Value>`, which lifts the `numeric_value` closure out of `bump`;
  - `fn column_type(&self, col: usize) -> ColumnType`, which lifts `ty_of`;
  - `fn write_steps(&mut self, values: Vec<((usize, usize), Value, ColumnType, f64)>) -> Result<usize, String>`, which validates every result first and then writes. Document rows go through `Draft::set` by `cell_ref`, inserted rows through `set_row_cell` by label. It does not rebuild; the caller does;
  - `fn step_selection_cells(&mut self, delta_of: impl Fn(usize, ColumnType) -> Option<f64>) -> Result<(usize, Skips), String>`, the shared collector used by `:bump` and the live step. `delta_of(col, ty)` answers `None` for a non-`Number` column.

- [ ] **Step 1: Write the failing tests**

```rust
// commands.rs tests: replace the Row default expectations
assert_eq!(parse("bump 0.25"), Ok(Command::Bump { delta: 0.25, axis: None }));
assert_eq!(parse("bump 0.25 row"), Ok(Command::Bump { delta: 0.25, axis: Some(BumpAxis::Row) }));
assert_eq!(parse("bump 1 col"), Ok(Command::Bump { delta: 1.0, axis: Some(BumpAxis::Col) }));
```

```rust
// tile/tests/selection.rs
#[gpui::test]
fn bump_with_no_axis_moves_every_selected_number_and_keeps_the_selection(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "right", Some(SLICE as u32));
    h.dispatch(&mut vcx, "visual_block", None);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "right", None);
    h.command(&mut vcx, "bump 0.01").unwrap();
    assert_eq!(h.row_texts(&vcx, 0)[3..], ["0.1100", "0.2100", "0.3000"]);
    assert_eq!(h.row_texts(&vcx, 1)[3..], ["0.4100", "0.5100", "0.6000"]);
    assert_eq!(h.mode(&vcx), "visual");
    // `row` keeps today's meaning even with a selection live.
    h.command(&mut vcx, "bump 1 row").unwrap();
    assert_eq!(h.row_texts(&vcx, 1)[0], "4510.00", "a row bump never moves the slice values");
}

#[gpui::test]
fn a_rows_selection_bump_skips_the_slice_values(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.command(&mut vcx, "bump 0.01").unwrap();
    assert_eq!(h.row_texts(&vcx, 0), vec!["4500.00", "0.1800", "-1.0000", "0.1100", "0.2100", "0.3100"]);
}

/// A flat panel restored with one inserted row (`new-1`, after `D1`),
/// whose amount is then typed — `a_commit_on_an_inserted_row_writes_its_own_cells`'s
/// own setup. The inserted row is model row 1.
fn open_with_inserted_row(cx: &mut gpui::TestAppContext) -> (Harness, gpui::VisualTestContext) {
    let restored: toml::Table = format!(
        r#"
key = ["SPX.Z"]
[draft]
base = "{BASE}"
edits = []
[draft.rows.new-1]
after = "D1"
cells = {{ ex = {{ type = "date", value = "2027-01-15" }}, status = {{ type = "text", value = "declared" }} }}
"#
    )
    .parse()
    .unwrap();
    let (h, mut vcx) = open_spec(cx, &test_fixtures::SCHEDULE, Some(restored));
    h.visible(&mut vcx, true);
    let tag = h.document_request().unwrap().tag;
    h.deliver(
        &mut vcx,
        tag,
        Arc::new(test_fixtures::schedule_snapshot(&[
            ("D1", "2026-12-18", 1.25, "declared"),
            ("D2", "2027-03-19", 0.5, "estimated"),
        ])),
    );
    h.tile.update(&mut vcx, |t, cx| t.cursor_to(1, Some(1), cx));
    h.dispatch(&mut vcx, "edit", None);
    h.set_editor(&mut vcx, "2.5");
    h.dispatch(&mut vcx, "commit", None);
    (h, vcx)
}

/// Two numeric columns of different declared types — `amount` (F64) and
/// `units` (I64) — over one row, for the all-or-nothing and per-column
/// step rules. `cell_ref`s are `(0, 0)` amount and `(0, 1)` units.
const SCHEDULE_MIXED: PanelSpec = PanelSpec {
    kind: "sched_mixed",
    title: "Mixed",
    dataset: "div_schedule_mixed",
    document: "div_schedule_mixed",
    rows: RowAxis { column: "dividend_id", identity: RowIdentity::Minted, label: RowLabel::Shown },
    columns: Columns::Values(&[
        ValueColumn { column: "amount", label: "amount", ty: ColumnType::F64, format: ColumnFormat::MEASURE, choices: None, required: true },
        ValueColumn { column: "units", label: "units", ty: ColumnType::I64, format: ColumnFormat::MEASURE, choices: None, required: true },
    ]),
    header: &[],
    slice_values: &[],
    value_type: ColumnType::F64,
    format: ColumnFormat::MEASURE,
    actions: &[],
};

fn open_mixed(cx: &mut gpui::TestAppContext) -> (Harness, gpui::VisualTestContext) {
    let (h, mut vcx) = open_spec(cx, &SCHEDULE_MIXED, None);
    h.command(&mut vcx, "key SPX.Z").unwrap();
    h.visible(&mut vcx, true);
    let tag = h.document_request().unwrap().tag;
    let snapshot = Snapshot::for_tests_with_provenance(
        vec![
            (meta("underlying_ref", Attribution::Additive), TestColumn::Dict(vec![Some("SPX.Z".into())])),
            (meta("dividend_id", Attribution::Additive), TestColumn::Dict(vec![Some("D1".into())])),
            (meta("amount", Attribution::DeterminedNonAdditive), TestColumn::F64(vec![1.25])),
            (meta("units", Attribution::DeterminedNonAdditive), TestColumn::I64(vec![1])),
        ],
        0,
        provenance(BASE),
    );
    h.deliver(&mut vcx, tag, Arc::new(snapshot));
    (h, vcx)
}

#[gpui::test]
fn a_selection_bump_reaches_an_inserted_rows_cells(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with_inserted_row(cx);
    h.dispatch(&mut vcx, "up", None); // D1
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "down", None); // D1 + new-1
    h.command(&mut vcx, "bump 1").unwrap();
    assert_eq!(h.cell(&vcx, 0, 1).0, "2.2500");
    assert_eq!(h.cell(&vcx, 1, 1).0, "3.5000");
    let inserted = h.tile.read_with(&vcx, |t, _| t.draft().row_state("new-1").cloned());
    assert!(
        matches!(&inserted, Some(RowEdit::Inserted { cells, .. }) if cells.get("amount") == Some(&Value::F64(3.5))),
        "the inserted row's cell is written by label, not by position: {inserted:?}"
    );
}

#[gpui::test]
fn a_fractional_bump_over_a_mixed_block_writes_nothing(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_mixed(cx);
    h.dispatch(&mut vcx, "visual_rows", None); // amount (F64) and units (I64)
    let refused = h.command(&mut vcx, "bump 0.5").unwrap_err();
    assert!(refused.contains("whole numbers"), "{refused}");
    assert!(
        h.tile.read_with(&vcx, |t, _| t.draft().is_empty()),
        "amount accepted 0.5, but units refused it, so nothing is written"
    );
}
```

`meta`, `provenance`, `TestColumn`, `Snapshot`, `Attribution`, `RowEdit`, `ValueColumn`, `RowAxis`, `RowIdentity`, `RowLabel` and `ColumnFormat` come from `tile.rs`'s test module (`use super::*`). Add a `use` for any that module imports under another path. `SCHEDULE_I64` (`tile.rs:8546`) is the model for `SCHEDULE_MIXED`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p geode-marketdata --lib -- bump`
Expected: compile error on `axis: None`, then FAIL.

- [ ] **Step 3: Implement**

- `commands.rs`: `axis: Option<BumpAxis>`. `None` means no word, `Some(Row)` means `row`, `Some(Col)` means `col`. Update the `Command::Bump` doc: "no axis: the selection when one is live, else the cursor's row". Update the usage string: `usage: bump <delta> [row|col]`.
- `tile.rs` `bump(delta, axis: Option<BumpAxis>, cx)`:
  - When `axis.is_none() && self.selection.is_some()`, return `self.bump_selection(delta, cx)`.
  - Otherwise continue with `axis.unwrap_or_default()` through today's body, refactored so its tail is:

```rust
let n = self.write_steps(values.into_iter().map(|(cell, v, ty)| (cell, v, ty, delta)).collect())?;
let _ = n;
self.rebuild_model(cx);
self.changed(cx);
Ok(())
```

- `write_steps` (in `select.rs`, since both paths use it) holds today's document/inserted split, validate-all-then-write, and `Draft::bump`/`set_row_cell` calls, generalised to a per-cell delta:

```rust
pub(super) fn write_steps(&mut self, values: Vec<((usize, usize), Value, ColumnType, f64)>) -> Result<usize, String> {
    let base = self.edit_base()?;
    let mut writes: Vec<((usize, usize), (String, String), Value)> = Vec::with_capacity(values.len());
    for (cell, current, ty, delta) in values {
        let labels = self.model.label_of(cell);
        let labels = (labels.0.to_string(), labels.1.to_string());
        // Every result is computed before any write: one refusal leaves
        // the whole draft as it was (`Draft::bump`'s contract).
        let value = bumped(&current, delta, ty, &labels.1)?;
        writes.push((cell, labels, value));
    }
    let n = writes.len();
    for (cell, labels, value) in writes {
        let row = &self.model.rows[cell.0];
        match row.state {
            RowState::Inserted => {
                self.draft.set_row_cell(&labels.0, &labels.1, value);
            }
            RowState::Document | RowState::Deleted => {
                let cell_ref = row.cells[cell.1].cell_ref;
                self.draft.set(cell_ref, labels, value, &base);
            }
        }
    }
    Ok(n)
}
```

  `Draft::bump` loses its only caller. Delete it and move its three tests to `write_steps` coverage, or keep it if other callers exist (`grep -n "\.bump(" crates/geode-marketdata`). The draft-level `bumped` tests stay.

- `step_selection_cells` (in `select.rs`):

```rust
/// Collect and write one step over the selection: every member on a
/// live row whose column is a number and whose value is not NULL, each
/// moved by `delta_of(col, ty)`. Answers how many were written and why
/// the rest were not. Gates: `held_refusal`, then `edit_base`.
pub(super) fn step_selection_cells(
    &mut self,
    delta_of: impl Fn(usize, ColumnType) -> Option<f64>,
) -> Result<(usize, Skips), String> {
    if let Some(refusal) = self.held_refusal() {
        return Err(refusal.to_string());
    }
    self.edit_base()?;
    let mut skips = Skips::default();
    let mut values = Vec::new();
    for (row, col) in self.selection_cells() {
        if self.model.rows[row].state == RowState::Deleted {
            skips.add(Skip::Deleted);
            continue;
        }
        let ty = self.column_type(col);
        let Some(delta) = delta_of(col, ty) else {
            skips.add(Skip::NotNumeric);
            continue;
        };
        match self.current_numeric(row, col) {
            Some(v) => values.push(((row, col), v, ty, delta)),
            None => skips.add(Skip::Empty),
        }
    }
    if values.is_empty() {
        return Err(format!("no numeric cells to step{}", skips.describe()));
    }
    let n = self.write_steps(values)?;
    Ok((n, skips))
}

fn bump_selection(&mut self, delta: f64, cx: &mut Context<Self>) -> Result<(), String> {
    let kinds = self.model.column_kinds.clone();
    let (n, skips) = self.step_selection_cells(|col, _| {
        matches!(kinds.get(col), Some(CellKind::Number(_))).then_some(delta)
    })?;
    self.rebuild_model(cx);
    self.notice = Some(format!("bumped {n} cells{}", skips.describe()).into());
    self.changed(cx);
    Ok(())
}
```

  (`column_kinds` is `pub` on `MatrixModel`; if `kind_of` does more than index it, call `self.model.kind_of(col)` inside instead and avoid the clone by collecting `(col, is_number)` first.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p geode-marketdata`
Expected: PASS. Every existing `bump_*` test keeps passing unchanged, which proves the refactor.

- [ ] **Step 5: Commit**

```bash
git commit -am "feat(marketdata): :bump with no axis acts on the selection

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: A typed value commits to every accepting selected cell

**Files:**
- Modify: `crates/geode-marketdata/src/tile.rs` (`commit_cell_edit`, the date-cell arm of `commit_edit`, `pick_option`), `crates/geode-marketdata/src/tile/select.rs`
- Test: `crates/geode-marketdata/src/tile/tests/selection.rs`

**Interfaces:**
- Consumes: `bulk::{accept, Skips, set_notice}`, `selection_cells`, `column_required`.
- Produces: `fn commit_bulk(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) -> bool`. It returns "header needs re-preparing". It refuses with the editor still open when nothing accepts. On success it writes, closes the editor, rebuilds and sets the notice. It keeps the selection.

- [ ] **Step 1: Write the failing tests**

```rust
#[gpui::test]
fn i_over_a_block_writes_one_value_to_every_accepting_cell(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "right", Some(SLICE as u32));
    h.dispatch(&mut vcx, "visual_block", None);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "right", None);
    h.dispatch(&mut vcx, "edit", None);
    assert_eq!(h.mode(&vcx), "insert");
    h.set_editor(&mut vcx, "0.25");
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(h.row_texts(&vcx, 0)[3..], ["0.2500", "0.2500", "0.3000"]);
    assert_eq!(h.row_texts(&vcx, 1)[3..], ["0.2500", "0.2500", "0.6000"]);
    assert!(h.header_texts(&vcx).iter().any(|t| t == "set 4 cells"));
    assert_eq!(h.mode(&vcx), "visual", "a block commit keeps the selection");
    h.command(&mut vcx, "revert").unwrap();
    assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_empty()), "one revert restores all of it");
}

#[gpui::test]
fn a_flat_block_commit_skips_cells_that_refuse_and_counts_them(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_flat(cx);
    h.with_flat_document(&mut vcx);
    h.dispatch(&mut vcx, "right", None); // amount
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "edit", None);
    h.set_editor(&mut vcx, "2");
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(h.row_texts(&vcx, 0), vec!["2026-12-18", "2.0000", "declared"]);
    assert_eq!(h.row_texts(&vcx, 1), vec!["2027-03-19", "2.0000", "estimated"]);
    assert!(
        h.header_texts(&vcx)
            .contains(&"set 2 cells, skipped 4 (2 wrong type, 2 not an option)".to_string())
    );
}

#[gpui::test]
fn a_block_commit_nothing_accepts_is_refused_with_the_editor_open(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "visual_block", None);
    h.dispatch(&mut vcx, "edit", None);
    h.set_editor(&mut vcx, "abc");
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(h.mode(&vcx), "insert");
    assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_empty()));
}

#[gpui::test]
fn a_choice_pick_over_a_selection_writes_the_option_to_every_choice_cell(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_flat(cx);
    h.with_flat_document(&mut vcx);
    h.dispatch(&mut vcx, "right", Some(2)); // status
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "edit", None);
    assert!(h.tile.read_with(&vcx, |t, _| t.choice_popup_open()));
    h.set_choice_text(&mut vcx, "paid");
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(h.col_texts(&vcx, 2), vec!["paid", "paid"]);
    assert_eq!(h.col_texts(&vcx, 1), vec!["1.2500", "0.5000"], "amount refused 'paid'");
    assert!(
        h.header_texts(&vcx)
            .contains(&"set 2 cells, skipped 4 (4 wrong type)".to_string())
    );
    assert!(!h.tile.read_with(&vcx, |t, _| t.choice_popup_open()));
}

#[gpui::test]
fn a_date_commit_over_a_selection_writes_every_date_cell(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_flat(cx);
    h.with_flat_document(&mut vcx);
    h.dispatch(&mut vcx, "visual_block", None); // ex column
    h.dispatch(&mut vcx, "down", None); // cursor on D2's 2027-03-19
    h.dispatch(&mut vcx, "edit", None);
    draw(&mut vcx);
    type_keys(&mut vcx, "2"); // the day segment: 2027-03-02
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(h.col_texts(&vcx, 0), vec!["2027-03-02", "2027-03-02"]);
    assert!(h.header_texts(&vcx).contains(&"set 2 cells".to_string()));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p geode-marketdata --lib -- selection::` (the new names)
Expected: FAIL (only the cursor cell is written).

- [ ] **Step 3: Implement**

```rust
// tile/select.rs
/// `enter` on a typed value with a selection live (spec §4.4): write it
/// to every selected cell whose kind accepts it, skipping and counting
/// the rest. Nothing accepts → refused with the editor open and the
/// draft untouched. Live steps since `i` are dropped first (restored to
/// the draft as it was), so a cell that refuses the typed value returns
/// to its pre-`i` value rather than keeping a half-step.
pub(super) fn commit_bulk(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
    if let Some(refusal) = self.held_refusal() {
        self.notice = Some(refusal.into());
        return true;
    }
    let base = match self.edit_base() {
        Ok(b) => b,
        Err(e) => {
            self.notice = Some(e.into());
            return true;
        }
    };
    let mut skips = Skips::default();
    let mut writes = Vec::new();
    for (row, col) in self.selection_cells() {
        if self.model.rows[row].state == RowState::Deleted {
            skips.add(Skip::Deleted);
            continue;
        }
        let Some(kind) = self.model.kind_of(col) else { continue };
        let ty = declared_type(self.spec, &self.model, col);
        match accept(kind, ty, self.column_required(col), text) {
            Ok(value) => writes.push(((row, col), value)),
            Err(skip) => skips.add(skip),
        }
    }
    if writes.is_empty() {
        self.notice = Some(format!("no selected cell accepts '{}'{}", text.trim(), skips.describe()).into());
        return true;
    }
    // Drop any live steps before writing (Task 7 fills `bulk`).
    if let Some(bulk) = self.editor.as_mut().and_then(|e| e.bulk.take())
        && bulk.stepped
    {
        self.draft.restore_from(bulk.before);
    }
    let n = writes.len();
    for ((row, col), value) in writes {
        let labels = self.model.label_of((row, col));
        let m = &self.model.rows[row];
        match m.state {
            RowState::Inserted => {
                self.draft.set_row_cell(labels.0.as_ref(), labels.1.as_ref(), value);
            }
            RowState::Document | RowState::Deleted => {
                let cell_ref = m.cells[col].cell_ref;
                self.draft.set(cell_ref, (labels.0.to_string(), labels.1.to_string()), value, &base);
            }
        }
    }
    self.close_editor(window, cx);
    self.close_popup_with_window(window, cx);
    self.rebuild_model(cx);
    self.notice = Some(set_notice(n, &skips).into());
    true
}
```

Until Task 7 adds `Editing.bulk`, write this function without the `bulk` block, and add the block in Task 7. `restore_from` runs before the writes, and the rows are unchanged by it: labels and `cell_ref`s come from the model, which is rebuilt only afterwards, and a restore never changes row structure because steps never insert rows.

Hooks (each one line at the top of the existing function, guarded by `self.selection.is_some()`):
- `commit_cell_edit(cell, labels, text, …)`: `if self.selection.is_some() { return self.commit_bulk(text, window, cx); }`.
- `commit_edit`'s `(EditorState::Date{..}, EditTarget::Cell{..})` arm: after `complete_pending` succeeds, `if self.selection.is_some() { let text = field.date().format("%Y-%m-%d").to_string(); return self.commit_bulk(&text, window, cx); }`. Bind `text` before calling to release the `field` borrow.
- `pick_option(option, …)`: after reading the target, `if self.selection.is_some() { return self.commit_bulk(&option, window, cx); }`. `commit_bulk` closes the popup itself.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p geode-marketdata`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git commit -am "feat(marketdata): one typed value commits to every accepting selected cell

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7: Live relative arrow step; one escape undoes it

**Files:**
- Modify: `crates/geode-marketdata/src/tile.rs` (`Editing`, `begin_edit`, `nudge`, `commit_edit` text-cell arm, `close_editor`), `crates/geode-marketdata/src/tile/select.rs`
- Test: `crates/geode-marketdata/src/tile/tests/selection.rs`

**Interfaces:**
- Consumes: `step_selection_cells`, `bulk::{step_delta, step_notice}`, `Draft::restore_from`.
- Produces:
  - `struct Bulk { before: Draft, painted: Option<DocumentBase>, seeded: String, steps: i64, stepped: bool }`;
  - `Editing.bulk: Option<Bulk>`;
  - `fn bulk_step(&mut self, steps: i64, window: &mut Window, cx: &mut Context<Self>) -> Option<bool>`, which answers `None` when the live step does not apply (no bulk, text typed, or cursor cell not a number) so `nudge` falls through to today's behaviour.

- [ ] **Step 1: Write the failing tests**

```rust
fn select_two_nodes_by_two_terms(h: &Harness, vcx: &mut gpui::VisualTestContext) {
    h.dispatch(vcx, "right", Some(SLICE as u32));
    h.dispatch(vcx, "visual_block", None);
    h.dispatch(vcx, "down", None);
    h.dispatch(vcx, "right", None);
    // cursor ends on row 1, col 4 (0.5000)
}

#[gpui::test]
fn arrows_step_every_selected_number_live_and_enter_keeps_them(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    select_two_nodes_by_two_terms(&h, &mut vcx);
    h.dispatch(&mut vcx, "edit", None);
    h.dispatch(&mut vcx, "insert_up", Some(2));
    h.dispatch(&mut vcx, "insert_up_big", None);
    assert_eq!(h.row_texts(&vcx, 0)[3..], ["0.1012", "0.2012", "0.3000"], "the grid paints the steps at once");
    assert_eq!(h.row_texts(&vcx, 1)[3..], ["0.4012", "0.5012", "0.6000"]);
    assert_eq!(h.editor_value(&vcx).as_deref(), Some("0.5012"), "the editor follows the cursor cell");
    assert!(h.header_texts(&vcx).iter().any(|t| t == "stepped 4 cells +12"));
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(h.mode(&vcx), "visual");
    assert_eq!(h.row_texts(&vcx, 1)[4], "0.5012");
}

#[gpui::test]
fn a_block_steps_each_column_at_its_own_places(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "visual_block", None); // anchor on fwd (2 places)
    h.dispatch(&mut vcx, "right", None);        // … through atm (4 places)
    h.dispatch(&mut vcx, "edit", None);
    h.dispatch(&mut vcx, "insert_down", None);
    assert_eq!(h.row_texts(&vcx, 0)[..2], ["4499.99", "0.1799"]);
}

#[gpui::test]
fn escape_after_steps_restores_the_draft_as_it_was_before_i(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.command(&mut vcx, "bump 1 col").unwrap(); // an earlier edit that must survive
    let before = h.tile.read_with(&vcx, |t, _| t.draft().clone());
    select_two_nodes_by_two_terms(&h, &mut vcx);
    h.dispatch(&mut vcx, "edit", None);
    h.dispatch(&mut vcx, "insert_up", Some(5));
    h.dispatch(&mut vcx, "cancel", None);
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.draft().clone()), before);
    assert_eq!(h.mode(&vcx), "visual");
}

#[gpui::test]
fn a_mixed_block_steps_each_column_by_its_own_unit(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_mixed(cx);
    h.dispatch(&mut vcx, "visual_rows", None); // cursor on amount (F64)
    h.dispatch(&mut vcx, "edit", None);
    h.dispatch(&mut vcx, "insert_up", None);
    let unit = step_delta(ColumnType::F64, ColumnFormat::MEASURE.precision, 1);
    let edits = h.tile.read_with(&vcx, |t, _| t.draft().edits.clone());
    assert_eq!(edits.get(&(0, 0)), Some(&Value::F64(1.25 + unit)), "amount by its places");
    assert_eq!(edits.get(&(0, 1)), Some(&Value::I64(2)), "units by one whole unit");
}

#[gpui::test]
fn typing_after_steps_replaces_them_and_a_refusing_cell_returns_to_before(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_mixed(cx);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "edit", None);
    h.dispatch(&mut vcx, "insert_up", None); // amount and units both stepped
    h.set_editor(&mut vcx, "2.5"); // typed: absolute from here
    h.dispatch(&mut vcx, "commit", None);
    let edits = h.tile.read_with(&vcx, |t, _| t.draft().edits.clone());
    assert_eq!(edits.get(&(0, 0)), Some(&Value::F64(2.5)), "the typed value replaced the step");
    assert_eq!(edits.get(&(0, 1)), None, "units refused 2.5 and is back to its pre-`i` value, no half-step");
    assert!(
        h.header_texts(&vcx)
            .contains(&"set 1 cell, skipped 1 (1 wrong type)".to_string())
    );
}

#[gpui::test]
fn after_typing_arrows_nudge_only_the_text(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    select_two_nodes_by_two_terms(&h, &mut vcx);
    h.dispatch(&mut vcx, "edit", None);
    h.set_editor(&mut vcx, "0.3000");
    h.dispatch(&mut vcx, "insert_up", None);
    assert_eq!(h.editor_value(&vcx).as_deref(), Some("0.3001"));
    assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_empty()), "typed: nothing live");
}

#[gpui::test]
fn escape_after_steps_keeps_a_behind_that_arrived_meanwhile(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    let tag = h.with_document_tagged(&mut vcx);
    // An earlier edit, so the restored draft is non-empty and can stay Behind.
    h.command(&mut vcx, "bump 1 col").unwrap(); // fwd, both terms
    select_two_nodes_by_two_terms(&h, &mut vcx);
    h.dispatch(&mut vcx, "edit", None);
    h.dispatch(&mut vcx, "insert_up", None);
    // A newer generation lands under Hold → Behind; the base stays painted.
    h.deliver(&mut vcx, tag, Arc::new(document_of(&TERMS, &NODES, NEWER)));
    assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_behind()));
    h.dispatch(&mut vcx, "cancel", None);
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.draft().len()), 2, "only the fwd bump remains");
    assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_behind()), "and the delivery is still news");
}

#[gpui::test]
fn the_selection_edit_refuses_while_behind(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    let tag = h.with_document_tagged(&mut vcx);
    h.command(&mut vcx, "bump 1 col").unwrap();
    h.deliver(&mut vcx, tag, Arc::new(document_of(&["2026-11-20"], &NODES, NEWER)));
    assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_behind()));
    h.dispatch(&mut vcx, "visual_block", None);
    h.dispatch(&mut vcx, "edit", None);
    assert!(h.editor_value(&vcx).is_none(), "no editor opens while behind");
    assert_eq!(h.mode(&vcx), "visual");
    assert_eq!(
        h.command(&mut vcx, "bump 1"),
        Err("the draft is behind — :rebase or :revert first".to_string())
    );
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.draft().len()), 2, "nothing further was written");
}
```


- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p geode-marketdata --lib -- selection::`
Expected: FAIL (arrows only nudge the editor text).

- [ ] **Step 3: Implement**

`tile.rs`:
- Add `bulk: Option<Bulk>` to `Editing` (set `None` at every construction site).
- Define `Bulk` beside `Editing`:

```rust
/// A cell editor opened on a live selection (grid selection spec §4.4).
/// While its text is untouched, arrows step every selected number in
/// the draft at once; `escape` puts `before` back.
struct Bulk {
    /// The draft when `i` opened the editor.
    before: Draft,
    /// The generation painted then. An escape after it moved (auto
    /// rebase, replace) keeps the steps: `before` is keyed to a grid
    /// that is no longer painted.
    painted: Option<DocumentBase>,
    /// The text the tile last put in the editor. The trader's own typing
    /// makes the value differ, which turns the edit absolute.
    seeded: String,
    /// Signed steps since `i`, for the notice.
    steps: i64,
    stepped: bool,
}
```

- `begin_edit`: when building `Editing` for `EditTarget::Cell` with `self.selection.is_some()` and `EditorState::Text`, set:

```rust
bulk: Some(Bulk {
    before: self.draft.clone(),
    painted: self.model.base.clone(),
    seeded: text.to_string(),
    steps: 0,
    stepped: false,
})
```

  (The `text` is the seed you already have.) Date cells get `None`: the date field has its own segment stepping and an absolute commit.
- `nudge`: first line `if let Some(outcome) = self.bulk_step(steps, window, cx) { return outcome; }`.
- `commit_edit`'s `(EditorState::Text(state), EditTarget::Cell { .. })` arm, before `commit_cell_edit`:

```rust
if let Some(bulk) = &editing.bulk
    && text == bulk.seeded
{
    // Untouched text: the live steps are the edit. Keep them.
    editing.bulk = None;
    self.close_editor(window, cx);
    return true;
}
```

  Pull `text` out first, and reborrow `self.editor` as needed to satisfy the borrow checker.
- `close_editor`: before the blur, take any bulk that has stepped and restore:

```rust
if let Some(bulk) = self.editor.as_mut().and_then(|e| e.bulk.take())
    && bulk.stepped
{
    if bulk.painted == self.model.base {
        self.draft.restore_from(bulk.before);
        if !self.draft.is_behind() {
            self.leave_behind();
        }
        self.rebuild_model(cx);
    } else {
        self.notice = Some("steps kept: the document moved".into());
    }
}
```

  Every commit path that should keep the steps takes `bulk` first (the untouched `enter` above and `commit_bulk` in Task 6, which now gets its `bulk` block), so `close_editor` means cancel by default: `escape`, a click elsewhere, the menu, a row verb.

`tile/select.rs`:

```rust
/// The editor's arrow keys with a selection live and its text untouched
/// (spec §4.4): step every selected number by its own column's unit, in
/// the draft, now. `None` when this is not that case — typed text, a
/// non-number cursor cell, no selection — so `nudge` keeps its own
/// behaviour. `Some(chrome)` otherwise.
pub(super) fn bulk_step(&mut self, steps: i64, window: &mut Window, cx: &mut Context<Self>) -> Option<bool> {
    let editing = self.editor.as_ref()?;
    let bulk = editing.bulk.as_ref()?;
    let EditorState::Text(state) = &editing.state else { return None };
    let EditTarget::Cell { cell, .. } = editing.target.clone() else { return None };
    let state = state.clone();
    if state.read(cx).value().as_ref() != bulk.seeded {
        return None;
    }
    if !matches!(self.model.kind_of(cell.1), Some(CellKind::Number(_))) {
        return None;
    }
    let precisions: Vec<Option<u8>> = (0..self.model.columns.len())
        .map(|c| match self.model.kind_of(c) {
            Some(CellKind::Number(f)) => Some(f.precision),
            _ => None,
        })
        .collect();
    let result = self.step_selection_cells(|col, ty| {
        precisions.get(col).copied().flatten().map(|p| step_delta(ty, p, steps))
    });
    match result {
        Ok((n, skips)) => {
            self.rebuild_model(cx);
            let text = self.model.rows[cell.0].cells[cell.1].text.to_string();
            state.update(cx, |s, cx| s.set_value(text.clone(), window, cx));
            let bulk = self.editor.as_mut()?.bulk.as_mut()?;
            bulk.seeded = text;
            bulk.steps += steps;
            bulk.stepped = true;
            self.notice = Some(step_notice(n, bulk.steps, &skips).into());
        }
        Err(e) => self.notice = Some(e.into()),
    }
    Some(true)
}
```

Keep the editor open across `rebuild_model`. It already survives rebuilds (commit identity is checked by labels). If `rebuild_model` calls `sync_cursor`, which mirrors the editor, nothing more is needed. `precision` is a `u8` on `ColumnFormat`; check the field's type and convert if it differs.

Update `commit_bulk` (Task 6) with its `bulk` block as written there.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p geode-marketdata`
Expected: PASS, including every existing nudge test (the single-cell path is unchanged because `bulk` is `None` without a selection).

- [ ] **Step 5: Commit**

```bash
git commit -am "feat(marketdata): arrows step a selection live; escape undoes the steps

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 8: Mouse — shift+click and drag

**Files:**
- Modify: `crates/geode-marketdata/src/delegate.rs`, `crates/geode-marketdata/src/tile.rs` (subscription + `pointer`)
- Test: `crates/geode-marketdata/src/tile/tests/selection.rs`

**Interfaces:**
- Produces:
  - `delegate::CellPointer::{Press { row: usize, col: Option<usize>, shift: bool }, Drag { row: usize, col: Option<usize>, label: bool }}`, where `col` is a model column and `None` is the row-label column;
  - `impl EventEmitter<CellPointer> for TableState<MatrixDelegate>`;
  - `MarketDataTile::pointer(&mut self, event: CellPointer, window, cx)`.

The blotter's `wire_pointer` (`crates/geode-blotter/src/delegate.rs:1047`) and `pointer` (`crates/geode-blotter/src/tile.rs:1038`) are the reference, including `drag_origin` / `drag_last` and the `on_mouse_up_out` release. Mirror them. In the mapping, the blotter's gutter corresponds to this panel's row-label cell (and the line-number gutter, which is painted inside the label cell).

- [ ] **Step 1: Probe the ordering (write this test first; it decides where the anchor comes from)**

```rust
#[gpui::test]
fn shift_click_anchors_at_the_cursor_and_extends_a_block_to_the_click(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "right", Some(SLICE as u32)); // cursor on (0, 3)
    shift_press(&mut vcx, "marketdata-cell-1-6"); // model (1, 5)
    assert_eq!(resolved(&h, &vcx).map(|r| (r.1, r.2)), Some((0..2, 3..6)));
    // Focus stayed on the tile: a typed key still reaches it.
    h.dispatch(&mut vcx, "yank", None);
    assert!(clipboard(&mut vcx).is_some_and(|t| t.lines().count() == 3));
}

#[gpui::test]
fn a_plain_click_clears_the_selection_and_moves_the_cursor(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "visual_block", None);
    let at = centre_of(&mut vcx, "marketdata-cell-1-6");
    click_at(&mut vcx, at, 1);
    assert_eq!(resolved(&h, &vcx), None);
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.cursor()), Cursor::Cell { row: 1, col: 5 });
}

fn drag(vcx: &mut gpui::VisualTestContext, from: &str, to: &str) {
    let (a, b) = (centre_of(vcx, from), centre_of(vcx, to));
    vcx.simulate_event(gpui::MouseDownEvent {
        position: a,
        modifiers: gpui::Modifiers::default(),
        button: gpui::MouseButton::Left,
        click_count: 1,
        first_mouse: false,
    });
    vcx.simulate_event(gpui::MouseMoveEvent {
        position: b,
        pressed_button: Some(gpui::MouseButton::Left),
        modifiers: gpui::Modifiers::default(),
    });
    vcx.simulate_event(gpui::MouseUpEvent {
        position: b,
        modifiers: gpui::Modifiers::default(),
        button: gpui::MouseButton::Left,
        click_count: 1,
    });
    draw(vcx);
}

fn shift_press(vcx: &mut gpui::VisualTestContext, selector: &str) {
    let at = centre_of(vcx, selector);
    let shift = gpui::Modifiers { shift: true, ..Default::default() };
    vcx.simulate_event(gpui::MouseDownEvent {
        position: at,
        modifiers: shift,
        button: gpui::MouseButton::Left,
        click_count: 1,
        first_mouse: false,
    });
    vcx.simulate_event(gpui::MouseUpEvent {
        position: at,
        modifiers: shift,
        button: gpui::MouseButton::Left,
        click_count: 1,
    });
}

#[gpui::test]
fn a_drag_across_cells_selects_a_block_from_the_press(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    // Table column 4 is model column 3 (the label column leads).
    drag(&mut vcx, "marketdata-cell-0-4", "marketdata-cell-1-6");
    assert_eq!(resolved(&h, &vcx), Some((SelectKind::Block, 0..2, 3..6)));
    click_at(&mut vcx, centre_of(&mut vcx, "marketdata-cell-0-1"), 1); // clears
    drag(&mut vcx, "marketdata-cell-0-0", "marketdata-cell-1-0");
    assert_eq!(
        resolved(&h, &vcx).map(|r| (r.0, r.1)),
        Some((SelectKind::Rows, 0..2)),
        "a drag that starts on a row label selects rows"
    );
}

#[gpui::test]
fn a_shift_click_on_a_row_label_selects_rows(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    shift_press(&mut vcx, "marketdata-cell-1-0");
    assert_eq!(resolved(&h, &vcx).map(|r| (r.0, r.1)), Some((SelectKind::Rows, 0..2)));
    assert_eq!(h.mode(&vcx), "visual");
}
```

The block expectation (3..6) assumes the component emits `SelectCell` after our child handler has already started the selection at the old cursor. If this probe fails because the anchor lands on the click, anchor from the pointer's press instead: record the pre-press cursor in the delegate on `on_mouse_down` capture (`capture_any_mouse_down`) and read it in `pointer`. Record the finding in the pointer doc comment.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p geode-marketdata --lib -- selection::`
Expected: FAIL.

- [ ] **Step 3: Implement**

`delegate.rs`:
- Add the `CellPointer` enum and `EventEmitter` impl.
- Add `drag_origin: Option<bool>` (true when the press landed on the label) and `drag_last: Option<(usize, Option<usize>)>` fields.
- Add `fn wire_pointer(el: Div, cx: &Context<TableState<Self>>, row: usize, col: Option<usize>) -> Div`, a line-for-line port of the blotter's with the `label` flag in place of `gutter`.
- Wrap both the value-arm and label-arm elements in `render_cell` with it.

`tile.rs`:
- Subscribe `cx.subscribe_in(&table, window, |this, _, e: &CellPointer, window, cx| this.pointer(*e, window, cx))`.
- `pointer`:
  - A plain press clears the selection; the table's `SelectCell` moves the cursor.
  - A shift press or a drag closes an open editor (a click is a cancel, `close_editor`'s rule).
  - If no selection is live, start one: kind `Rows` when the press was on the label, else `Block`.
  - Then move the cursor to `(row, col.unwrap_or(current col))` through `cursor_to`, which ends in `sync_cursor` and so refreshes the selection.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p geode-marketdata`
Expected: PASS, including every existing click and double-click test.

- [ ] **Step 5: Commit**

```bash
git commit -am "feat(marketdata): shift+click and drag select on the panel

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 9: Docs, mutation entries, gates

**Files:**
- Modify: `crates/geode-marketdata/README.md`, `docs/current/features.md` (Market-data documents section), `docs/current/keymaps.md` (the market-data context list), `scripts/mutation-check.sh`

- [ ] **Step 1: Docs.**
  - State the behaviour, why, the failure semantics, and the limitations: contiguous only, no paste, choice `space` step still acts on the cursor cell alone, the footer shows the extent only.
  - Describe the new `mode == visual` context, `select == rows|block`, and single-key `y`/`d`/`i` in visual mode.
  - Describe `:bump` with no axis on a selection.
  - List the five plan rulings as current behaviour, not as history.

- [ ] **Step 2: Mutation entries.** Append beside the existing `geode-marketdata` entries, in the file's `run_mutation "name" file 'anchor' 'replacement' package test_filter` form, one per contract:
  1. The block refusal. In `delete_selected_rows`, replace `SelectKind::Block)` with `SelectKind::Rows)` in the refusal guard. Test: `d_over_a_block_refuses_and_names_v`.
  2. Identity anchoring. In `refresh_selection`, replace `|label| self.model.rows.iter().position(|r| &r.label == label)` with `|_| Some(0)`. Test: `an_anchor_row_that_disappears_clears_the_selection_with_a_notice`.
  3. The escape restore. In `close_editor`, replace `self.draft.restore_from(bulk.before);` with `let _ = bulk.before;`. Test: `escape_after_steps_restores_the_draft_as_it_was_before_i`.
  4. Rows skip the slice values. In `selection_cells`, replace `SelectKind::Rows => self.model.slice_columns,` with `SelectKind::Rows => 0,`. Test: `a_rows_selection_bump_skips_the_slice_values`.
  5. Validate-before-write. In `write_steps`, move nothing; instead replace `let value = bumped(&current, delta, ty, &labels.1)?;` with `let Ok(value) = bumped(&current, delta, ty, &labels.1) else { continue };`. Test: `a_fractional_bump_over_a_mixed_block_writes_nothing`.

  Each anchor must be unique in its file. Run `zsh scripts/mutation-check.sh --anchors-only` and re-anchor any AMBIG.

- [ ] **Step 3: Run the gates**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo check -p geode-shell --features test-support --all-targets
zsh scripts/mutation-check.sh --anchors-only
zsh scripts/mutation-check.sh "marketdata: selection"   # use the names you gave the five entries
```

Expected: all green. Each of the five entries reports `caught`.

- [ ] **Step 4: Commit**

```bash
git add -A crates/geode-marketdata docs scripts/mutation-check.sh
git commit -m "docs(marketdata): selections, bulk edits and the live step; mutation entries

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

- [ ] **Step 5: Display check list for Matthew** (headless tests cannot see these):
  - block and rows tint over edited, sent and deleted cells, with the cursor border inside;
  - the footer extent strip;
  - the live step repaint while holding `shift+up`;
  - the shift+click and drag feel;
  - light and dark themes.
