# Grid selection, Part 1 (core + blotter) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the shared selection core, the footer aggregate strip, and the blotter's `V` (rows) and `v` (cell block) selections with TSV yank, per-column aggregates, and shift-click/drag.

**Architecture:** A pure `geode_core::grid::selection` module owns the selection type (anchored by row/column *identity*), index resolution, the top-most-rows filter, and aggregation. `geode_shell::shell::aggregates` paints a prepared strip. The blotter replaces its `Mode::Visual{anchor: usize}` with `Option<Selection<Path, String>>` on the delegate, re-resolves it at every change point, and stores the resolved ranges and the formatted summary so render only looks up.

**Tech Stack:** Rust, GPUI + gpui-component 0.6.2 `DataTable`, Criterion.

**Spec:** `docs/superpowers/specs/2026-09-26-grid-selection-design.md`. Parts 2 (market-data) and 3 (pricer) get their own plans after this merges; they consume Task 1's API unchanged.

## Global Constraints

- `geode-core` stays free of gpui and I/O.
- `geode-shell` never depends on `geode-blotter`; the blotter depends on both.
- No state mutation, I/O or unbounded work in `render`, `render_tr`, `render_td`.
- Theme tokens only: the selection tint is `theme.selection.opacity(0.35)` (today's value); no literal colours.
- Every pointer action has a keyboard route; mouse handlers never call `stop_propagation` (the shell's tile-focus press must still arrive).
- A `DeterminedNonAdditive` value "must never be totalled" (`geode_core::attribution`): a column containing one reports no sum and no mean.
- Row/column identity, not index: blotter rows are `Path` (`core::expansion::path_of`), columns are `PlannedColumn::name`.
- User-facing text says "color", not "colour"; code identifiers renamed only in files already touched.
- Every commit ends with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Gate before merge: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `zsh scripts/mutation-check.sh --anchors-only`.

## Deviations from the spec (flagged for the reviewer)

1. **Non-additive columns** (spec gap): §1 ruling 2 prevents tree double counting but not totalling a `DeterminedNonAdditive` measure across siblings, which the attribution model forbids. A column with any such contributing value shows `n`, `min`, `max` and `Σ —†`, never a sum or mean.
2. **No ordering probe** (spec §5): the handlers are written to be order-independent instead — the table's `SelectRow` only moves the cursor row and never clears a selection, and our own press event carries the column and the shift state — so no probe task is needed.

## Review Focus

1. **`V` then `G` on a 720k-row blotter** — each motion re-summarises the whole range; a person expects no visible lag. Owned by Task 6 (bench at 720,881 rows, recorded in `docs/current/performance.md`; budget 8 ms).
2. **Live refresh while selecting** — a snapshot delivery mid-selection must keep the tint on the same rows (by path), or clear with a notice if the anchor row went away. Owned by Task 4 (`a_redelivery_keeps_the_selection_on_the_same_rows`, `a_selection_whose_anchor_row_vanishes_clears_with_a_notice`).
3. **fzf-narrowed rows** — `/` narrowing shows rows in score order, not tree order; a selection over them must still never double count. Owned by Task 2 (`top_most_ignores_display_order`).
4. **Selecting only a collapsed group** — its hidden children must not be visited or summed twice. Owned by Task 2 (`a_collapsed_group_is_summed_as_itself`).
5. **Escape with find active** — first `escape` clears only the selection; a person expects their `/` narrowing to survive it. Owned by Task 4 (`escape_clears_the_selection_before_find`).

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/geode-core/src/grid/mod.rs` (create) | `pub mod selection;` |
| `crates/geode-core/src/grid/selection.rs` (create) | `SelectKind`, `Selection`, `Resolved`, `resolve`, `top_most`, `Accumulator`, `ColumnAggregate`, `describe` |
| `crates/geode-core/src/lib.rs` (modify) | `pub mod grid;` |
| `crates/geode-shell/src/shell/aggregates.rs` (create) | `strip(&[AggregateCell]) -> Div` element |
| `crates/geode-shell/src/shell/mod.rs` (modify) | `pub mod aggregates;` |
| `crates/geode-blotter/src/core/cursor.rs` (modify) | `find_by_path`; delete `Mode` and `selection` |
| `crates/geode-blotter/src/core/yank.rs` (modify) | `tsv` takes a column range |
| `crates/geode-blotter/src/core/select.rs` (create) | `summarize` — the blotter's top-most + per-column aggregate over a `Resolved` |
| `crates/geode-blotter/src/core/mod.rs` (modify) | `pub mod select;` |
| `crates/geode-blotter/src/delegate.rs` (modify) | selection fields, `refresh_selection`, tint, `CellPointer` emission |
| `crates/geode-blotter/src/tile.rs` (modify) | actions, dispatch, key context, footer strip, pointer handling, notice |
| `crates/geode-blotter/src/content.rs` (modify) | keymap |
| `crates/geode-blotter/benches/blotter.rs` (modify) | `summarize` bench at 720,881 rows |
| `scripts/mutation-check.sh` (modify) | entries |
| `docs/current/features.md`, `docs/current/keymaps.md`, `docs/current/performance.md`, `crates/geode-blotter/README.md`, `crates/geode-core/README.md` (modify) | docs |

---

### Task 1: Core selection model — types and resolution

**Files:**
- Create: `crates/geode-core/src/grid/mod.rs`, `crates/geode-core/src/grid/selection.rs`
- Modify: `crates/geode-core/src/lib.rs` (add `pub mod grid;` between `groupings` and `health`)

**Interfaces:**
- Produces:
  ```rust
  pub enum SelectKind { Rows, Block }                        // Copy, Eq, Debug
  pub struct Selection<R, C> { pub kind: SelectKind, pub anchor_row: R, pub anchor_col: C }
  pub struct Resolved { pub kind: SelectKind, pub rows: Range<usize>, pub cols: Range<usize> }
  impl Resolved { pub fn contains(&self, row: usize, col: usize) -> bool; pub fn contains_row(&self, row: usize) -> bool }
  pub fn resolve(kind: SelectKind, anchor: (usize, usize), cursor: (usize, usize), col_count: usize) -> Resolved
  impl<R, C> Selection<R, C> {
      pub fn resolve_with(&self, cursor: (usize, usize), col_count: usize,
          find_row: impl FnOnce(&R) -> Option<usize>, find_col: impl FnOnce(&C) -> Option<usize>) -> Option<Resolved>
  }
  ```

- [ ] **Step 1: Write the failing tests** in `selection.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_span_anchor_to_cursor_either_way_and_every_column() {
        let r = resolve(SelectKind::Rows, (5, 3), (2, 1), 4);
        assert_eq!((r.rows.clone(), r.cols.clone()), (2..6, 0..4));
        let r = resolve(SelectKind::Rows, (0, 0), (2, 3), 4);
        assert_eq!(r.rows, 0..3);
    }

    #[test]
    fn a_block_spans_the_rectangle_between_anchor_and_cursor() {
        let r = resolve(SelectKind::Block, (4, 3), (1, 1), 6);
        assert_eq!((r.rows.clone(), r.cols.clone()), (1..5, 1..4));
        assert!(r.contains(1, 1) && r.contains(4, 3));
        assert!(!r.contains(4, 4) && !r.contains(0, 2));
        assert!(r.contains_row(2));
    }

    #[test]
    fn a_block_column_range_is_clamped_to_the_column_count() {
        let r = resolve(SelectKind::Block, (0, 9), (0, 1), 4);
        assert_eq!(r.cols, 1..4);
    }

    #[test]
    fn resolution_goes_through_identity_and_a_lost_anchor_is_none() {
        let s = Selection { kind: SelectKind::Block, anchor_row: "b", anchor_col: "x" };
        let rows = ["a", "c", "b"]; // "b" moved from index 1 to 2 (a re-sort)
        let cols = ["w", "x"];
        let found = s.resolve_with((0, 0), 2,
            |r| rows.iter().position(|k| k == r),
            |c| cols.iter().position(|k| k == c)).unwrap();
        assert_eq!((found.rows, found.cols), (0..3, 0..2));
        let gone = Selection { kind: SelectKind::Block, anchor_row: "z", anchor_col: "x" };
        assert!(gone.resolve_with((0, 0), 2, |r| rows.iter().position(|k| k == r), |_| Some(0)).is_none());
    }

    #[test]
    fn a_rows_selection_never_needs_its_anchor_column() {
        let s = Selection { kind: SelectKind::Rows, anchor_row: 1usize, anchor_col: "hidden" };
        let r = s.resolve_with((3, 0), 5, |r| Some(*r), |_| None).unwrap();
        assert_eq!((r.rows, r.cols), (1..4, 0..5));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p geode-core grid::selection`
Expected: compile error (module/items missing).

- [ ] **Step 3: Implement**

`crates/geode-core/src/grid/mod.rs`:
```rust
//! Pure grid vocabulary shared by every tile that shows a table.

pub mod selection;
```

`crates/geode-core/src/grid/selection.rs` (head of file):
```rust
//! A grid selection (grid selection spec §3.1): whole rows (`V`) or a
//! rectangular block of cells (`v`), anchored by row and column
//! *identity* so a re-sort, a column move or a live redelivery keeps it
//! on the same data. Indices are recomputed from the anchor to the
//! cursor in the current display order on every change; an anchor that
//! is no longer displayed resolves to `None` and the tile clears the
//! selection rather than guessing a neighbour.

use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectKind {
    /// Whole rows; every column.
    Rows,
    /// A rectangle: a row range × a column range.
    Block,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection<R, C> {
    pub kind: SelectKind,
    pub anchor_row: R,
    pub anchor_col: C,
}

/// Display-index ranges, half-open. For `Rows`, `cols` is every column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub kind: SelectKind,
    pub rows: Range<usize>,
    pub cols: Range<usize>,
}

impl Resolved {
    pub fn contains_row(&self, row: usize) -> bool {
        self.rows.contains(&row)
    }

    pub fn contains(&self, row: usize, col: usize) -> bool {
        self.rows.contains(&row) && self.cols.contains(&col)
    }
}

fn span(a: usize, b: usize) -> Range<usize> {
    a.min(b)..a.max(b) + 1
}

pub fn resolve(
    kind: SelectKind,
    anchor: (usize, usize),
    cursor: (usize, usize),
    col_count: usize,
) -> Resolved {
    let rows = span(anchor.0, cursor.0);
    let cols = match kind {
        SelectKind::Rows => 0..col_count,
        SelectKind::Block => {
            let c = span(anchor.1, cursor.1);
            c.start.min(col_count)..c.end.min(col_count)
        }
    };
    Resolved { kind, rows, cols }
}

impl<R, C> Selection<R, C> {
    /// `None` when the anchor row — or, for a block, the anchor column —
    /// is not displayed. A `Rows` selection never looks its column up: a
    /// hidden anchor column must not drop a row selection.
    pub fn resolve_with(
        &self,
        cursor: (usize, usize),
        col_count: usize,
        find_row: impl FnOnce(&R) -> Option<usize>,
        find_col: impl FnOnce(&C) -> Option<usize>,
    ) -> Option<Resolved> {
        let row = find_row(&self.anchor_row)?;
        let col = match self.kind {
            SelectKind::Rows => 0,
            SelectKind::Block => find_col(&self.anchor_col)?,
        };
        Some(resolve(self.kind, (row, col), cursor, col_count))
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p geode-core grid::selection`
Expected: 5 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-core/src/grid crates/geode-core/src/lib.rs
git commit -m "feat(core): grid selection anchored by identity

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: Core — top-most rows, aggregation, description

**Files:**
- Modify: `crates/geode-core/src/grid/selection.rs`

**Interfaces:**
- Consumes: Task 1's module.
- Produces:
  ```rust
  pub fn top_most(rows: &[usize], universe: usize, parent: impl Fn(usize) -> Option<usize>) -> Vec<usize>
  #[derive(Debug, Clone, Copy, PartialEq, Default)]
  pub struct ColumnAggregate { pub count: usize, pub sum: Option<f64>, pub mean: Option<f64>,
                               pub min: Option<f64>, pub max: Option<f64>, pub non_additive: bool }
  #[derive(Debug, Clone, Default)] pub struct Accumulator { .. }
  impl Accumulator { pub fn add(&mut self, value: Option<f64>, additive: bool); pub fn finish(&self) -> ColumnAggregate }
  pub fn describe(agg: &ColumnAggregate, format: &crate::view::ColumnFormat, extremes: bool) -> String
  ```
  `rows` and `parent` speak the tile's dense row ids (the blotter: snapshot row numbers, `universe = snapshot.rows()`).

- [ ] **Step 1: Write the failing tests** (append to the tests module):

```rust
    // Tree: 0 root; 1, 2 children of 0; 3 child of 1; 4 child of 3.
    fn parent(r: usize) -> Option<usize> {
        [None, Some(0), Some(0), Some(1), Some(3)][r]
    }

    #[test]
    fn a_group_and_its_children_count_once_as_the_group() {
        assert_eq!(top_most(&[1, 3, 4, 2], 5, parent), vec![1, 2]);
    }

    #[test]
    fn some_children_alone_are_summed_themselves() {
        assert_eq!(top_most(&[3, 2], 5, parent), vec![3, 2]);
    }

    #[test]
    fn a_grandparent_hides_a_grandchild_even_without_the_middle_row() {
        assert_eq!(top_most(&[1, 4], 5, parent), vec![1]);
    }

    #[test]
    fn top_most_ignores_display_order() {
        // fzf narrowing shows rows in score order: child before parent.
        assert_eq!(top_most(&[4, 2, 1], 5, parent), vec![2, 1]);
    }

    #[test]
    fn a_collapsed_group_is_summed_as_itself() {
        // Row 1 collapsed: 3 and 4 are not displayed, so not in `rows`.
        assert_eq!(top_most(&[1], 5, parent), vec![1]);
    }

    #[test]
    fn nulls_and_nan_are_excluded_from_every_statistic() {
        let mut a = Accumulator::default();
        for v in [Some(1.0), None, Some(f64::NAN), Some(3.0)] {
            a.add(v, true);
        }
        let g = a.finish();
        assert_eq!(g.count, 2);
        assert_eq!(g.sum, Some(4.0));
        assert_eq!(g.mean, Some(2.0));
        assert_eq!((g.min, g.max), (Some(1.0), Some(3.0)));
    }

    #[test]
    fn a_non_additive_value_suppresses_sum_and_mean_but_not_extremes() {
        let mut a = Accumulator::default();
        a.add(Some(5.0), true);
        a.add(Some(7.0), false);
        let g = a.finish();
        assert!(g.non_additive);
        assert_eq!((g.sum, g.mean), (None, None));
        assert_eq!((g.count, g.min, g.max), (2, Some(5.0), Some(7.0)));
    }

    #[test]
    fn an_empty_column_has_no_statistics() {
        let g = Accumulator::default().finish();
        assert_eq!(g, ColumnAggregate::default());
    }

    #[test]
    fn describe_uses_the_column_format_and_marks_non_additive() {
        use crate::view::ColumnFormat;
        let mut a = Accumulator::default();
        a.add(Some(1000.0), true);
        a.add(Some(2000.0), true);
        let f = ColumnFormat::MEASURE;
        assert_eq!(describe(&a.finish(), &f, false), "Σ 3,000.00 · μ 1,500.00 · n 2");
        assert_eq!(
            describe(&a.finish(), &f, true),
            "Σ 3,000.00 · μ 1,500.00 · n 2 · min 1,000.00 · max 2,000.00"
        );
        let mut b = Accumulator::default();
        b.add(Some(1.0), false);
        assert_eq!(describe(&b.finish(), &f, false), "Σ —† · n 1");
        assert_eq!(describe(&Accumulator::default().finish(), &f, false), "n 0");
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p geode-core grid::selection`
Expected: compile errors for `top_most`, `Accumulator`, `ColumnAggregate`, `describe`.

- [ ] **Step 3: Implement** (append above the tests module):

```rust
/// The rows of `rows` with no ancestor also in `rows` (spec §1 ruling 2):
/// a parent row already carries its children's total, so counting both
/// would double it. Walks each row's ancestor chain against a dense
/// membership bitmap of size `universe` — independent of display order,
/// which fzf narrowing does not keep in tree order. Output keeps `rows`'
/// order.
pub fn top_most(
    rows: &[usize],
    universe: usize,
    parent: impl Fn(usize) -> Option<usize>,
) -> Vec<usize> {
    let mut selected = vec![false; universe];
    for &r in rows {
        if let Some(s) = selected.get_mut(r) {
            *s = true;
        }
    }
    rows.iter()
        .copied()
        .filter(|&r| {
            let mut at = parent(r);
            while let Some(p) = at {
                if selected.get(p).copied().unwrap_or(false) {
                    return false;
                }
                at = parent(p);
            }
            true
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ColumnAggregate {
    /// Non-NULL, non-NaN values seen.
    pub count: usize,
    /// `None` when `count == 0` or any value was non-additive.
    pub sum: Option<f64>,
    pub mean: Option<f64>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    /// Some contributing value must never be totalled
    /// (`Attribution::DeterminedNonAdditive`).
    pub non_additive: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Accumulator {
    count: usize,
    sum: f64,
    min: Option<f64>,
    max: Option<f64>,
    non_additive: bool,
}

impl Accumulator {
    pub fn add(&mut self, value: Option<f64>, additive: bool) {
        let Some(v) = value.filter(|v| !v.is_nan()) else {
            return;
        };
        self.count += 1;
        self.sum += v;
        self.min = Some(self.min.map_or(v, |m| m.min(v)));
        self.max = Some(self.max.map_or(v, |m| m.max(v)));
        self.non_additive |= !additive;
    }

    pub fn finish(&self) -> ColumnAggregate {
        let totals = self.count > 0 && !self.non_additive;
        ColumnAggregate {
            count: self.count,
            sum: totals.then_some(self.sum),
            mean: totals.then(|| self.sum / self.count as f64),
            min: self.min,
            max: self.max,
            non_additive: self.non_additive,
        }
    }
}

/// The footer text for one column (spec §3.3), formatted with that
/// column's own `ColumnFormat`. `extremes` adds `min`/`max` (a selection
/// covering a single numeric column).
pub fn describe(
    agg: &ColumnAggregate,
    format: &crate::view::ColumnFormat,
    extremes: bool,
) -> String {
    use crate::format::format_number;
    let f = |v: f64| format_number(v, format).text;
    let mut parts: Vec<String> = Vec::new();
    if agg.non_additive {
        parts.push("Σ —†".into());
    } else if let (Some(s), Some(m)) = (agg.sum, agg.mean) {
        parts.push(format!("Σ {}", f(s)));
        parts.push(format!("μ {}", f(m)));
    }
    parts.push(format!("n {}", agg.count));
    if extremes && let (Some(lo), Some(hi)) = (agg.min, agg.max) {
        parts.push(format!("min {}", f(lo)));
        parts.push(format!("max {}", f(hi)));
    }
    parts.join(" · ")
}
```

If `ColumnFormat::MEASURE` formats `3000.0` differently from `3,000.00` (check `crates/geode-core/src/format.rs` tests), change the *expected strings* to match `format_number`'s actual output for that format, not the implementation.

- [ ] **Step 4: Run tests**

Run: `cargo test -p geode-core grid::selection`
Expected: 14 passed.

- [ ] **Step 5: Add mutation entries** to `scripts/mutation-check.sh`, in a new section `# ---- grid selection (grid selection spec)` after the last geode-core section:

```zsh
run_mutation "grid selection: an ancestor in the selection hides the row" \
  crates/geode-core/src/grid/selection.rs \
  '                    return false;' \
  '                    return true;' \
  geode-core \
  a_group_and_its_children_count_once_as_the_group

run_mutation "grid selection: a non-additive value suppresses the sum" \
  crates/geode-core/src/grid/selection.rs \
  '        let totals = self.count > 0 && !self.non_additive;' \
  '        let totals = self.count > 0;' \
  geode-core \
  a_non_additive_value_suppresses_sum_and_mean_but_not_extremes

run_mutation "grid selection: a lost anchor row resolves to None" \
  crates/geode-core/src/grid/selection.rs \
  '        let row = find_row(&self.anchor_row)?;' \
  '        let row = find_row(&self.anchor_row).unwrap_or(0);' \
  geode-core \
  resolution_goes_through_identity_and_a_lost_anchor_is_none
```

Run: `zsh scripts/mutation-check.sh --anchors-only "grid selection"` → every entry `ok` (no ANCHOR/AMBIG/FILTER).
Then commit your work and run: `zsh scripts/mutation-check.sh "grid selection"` → every entry `caught`.

- [ ] **Step 6: Commit**

```bash
git add crates/geode-core/src/grid/selection.rs scripts/mutation-check.sh
git commit -m "feat(core): top-most rows and per-column selection aggregates

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: Shell aggregate strip

**Files:**
- Create: `crates/geode-shell/src/shell/aggregates.rs`
- Modify: `crates/geode-shell/src/shell/mod.rs` (add `pub mod aggregates;` alphabetically, before `pub mod asof_rows;`)

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, PartialEq)]
  pub struct AggregateCell { pub label: SharedString, pub text: SharedString }
  pub fn strip(cells: &[AggregateCell], theme: &gpui_component::Theme) -> gpui::Div
  ```
  The caller prepares `cells` outside render; `strip` only builds elements (one label + one text child per cell), cloning `SharedString`s (refcount bumps, no formatting).

- [ ] **Step 1: Write the failing test** (in `aggregates.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualContext as _};
    use gpui_component::ActiveTheme as _;

    struct Host(Vec<AggregateCell>);
    impl gpui::Render for Host {
        fn render(&mut self, _: &mut gpui::Window, cx: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
            strip(&self.0, cx.theme())
        }
    }

    #[gpui::test]
    fn each_cell_paints_its_label_and_text(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let cells = vec![
            AggregateCell { label: "delta".into(), text: "Σ 3.00 · n 2".into() },
            AggregateCell { label: "gamma".into(), text: "n 0".into() },
        ];
        let (_view, cx) = cx.add_window_view(|_, _| Host(cells));
        cx.run_until_parked();
        assert!(cx.debug_bounds("aggregate-delta").is_some());
        assert!(cx.debug_bounds("aggregate-gamma").is_some());
    }
}
```

Before writing, check how other `geode-shell` element tests initialise the theme (e.g. grep `add_window_view` in `crates/geode-shell/src/shell/kbd.rs` or `listrow.rs`) and copy that setup exactly if it differs from `gpui_component::init`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p geode-shell --features test-support shell::aggregates`
Expected: compile error.

- [ ] **Step 3: Implement**

```rust
//! The footer strip a grid tile shows while a selection is live (grid
//! selection spec §3.3): one `label text` pair per selected numeric
//! column. The tile formats every string when the selection or the data
//! changes; this only lays prepared strings out, so painting it per
//! frame formats nothing.

use gpui::prelude::*;
use gpui::{Div, SharedString, div};
use gpui_component::{Theme, h_flex};

#[derive(Debug, Clone, PartialEq)]
pub struct AggregateCell {
    pub label: SharedString,
    pub text: SharedString,
}

pub fn strip(cells: &[AggregateCell], theme: &Theme) -> Div {
    h_flex().gap_3().items_center().children(cells.iter().map(|c| {
        let label = c.label.clone();
        h_flex()
            .gap_1()
            .debug_selector(move || format!("aggregate-{label}"))
            .child(div().text_color(theme.muted_foreground).child(c.label.clone()))
            .child(div().text_color(theme.foreground).child(c.text.clone()))
    }))
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p geode-shell --features test-support shell::aggregates`
Expected: 1 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-shell/src/shell/aggregates.rs crates/geode-shell/src/shell/mod.rs
git commit -m "feat(shell): footer strip for selection aggregates

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: Blotter pure pieces — exact path lookup, block TSV, summary

**Files:**
- Modify: `crates/geode-blotter/src/core/cursor.rs`, `crates/geode-blotter/src/core/yank.rs`
- Create: `crates/geode-blotter/src/core/select.rs`; add `pub mod select;` to `crates/geode-blotter/src/core/mod.rs`

**Interfaces:**
- Consumes: `geode_core::grid::selection::{Resolved, top_most, Accumulator, describe}`.
- Produces:
  ```rust
  // cursor.rs
  pub fn find_by_path(visible: &[u32], snapshot: &Snapshot, plan: &ColumnPlan, path: &[Option<String>], near: usize) -> Option<usize>
  // yank.rs
  pub fn tsv(snapshot: &Snapshot, plan: &ColumnPlan, visible: &[u32], rows: Range<usize>, cols: Range<usize>) -> String
  // select.rs
  pub fn summarize(snapshot: &Snapshot, plan: &ColumnPlan, shown: &[u32], resolved: &Resolved) -> Vec<(String, String)>
  ```
  `summarize` returns `(column label, describe text)` per `ColumnKind::Measure` column in `resolved.cols`, in column order; `extremes` is true when exactly one measure column is in range.

- [ ] **Step 1: `find_by_path` — refactor `restore_by_path` around it.** Rename the body of `restore_by_path` into `find_by_path`, returning `Some(i)` where it returned a match and `None` where it fell back; then:

```rust
pub fn restore_by_path(visible: &[u32], snapshot: &Snapshot, plan: &ColumnPlan,
    path: &[Option<String>], fallback: usize) -> usize {
    find_by_path(visible, snapshot, plan, path, fallback)
        .unwrap_or_else(|| fallback.min(visible.len().saturating_sub(1)))
}
```
`find_by_path` returns `None` for an empty `visible`. Keep the I3 doc comment on `find_by_path` and give `restore_by_path` a one-line doc: "`find_by_path`, falling back to `fallback` clamped."

Test (in cursor.rs tests; reuse the module's existing snapshot fixture helper if it has one, else build a two-level `Snapshot::for_tests` as in `yank.rs`'s test):

```rust
    #[test]
    fn find_by_path_is_exact_and_restore_falls_back() {
        // fixture: shown rows [root, L1, L1/SPX]; path ["L2"] absent.
        let (snap, plan, shown) = fixture();
        let l1 = path_of(&snap, &plan, shown[1] as usize);
        assert_eq!(find_by_path(&shown, &snap, &plan, &l1, 0), Some(1));
        let missing = vec![Some("L2".to_string())];
        assert_eq!(find_by_path(&shown, &snap, &plan, &missing, 1), None);
        assert_eq!(restore_by_path(&shown, &snap, &plan, &missing, 1), 1);
        assert_eq!(find_by_path(&[], &snap, &plan, &l1, 0), None);
    }
```

- [ ] **Step 2: Delete `Mode` and `selection`** from `cursor.rs` and their test `a_visual_selection_spans_anchor_to_cursor_either_way` (its behaviour now lives in Task 1's `rows_span_anchor_to_cursor_either_way_and_every_column`). Keep `visual_mode_clamps_a_bare_step` (it tests `move_rows(.., wrap=false)`). Callers in `delegate.rs`/`tile.rs` break here and are fixed in Task 5; do not run the crate's tests until then — run only `cargo test -p geode-blotter --lib core::` after Step 5 below compiles the core (the `core` module does not depend on `delegate`/`tile`). If the lib fails to compile because of `tile.rs`/`delegate.rs`, combine this task's commit with Task 5 and say so in the commit message.

- [ ] **Step 3: `tsv` over a column range.** Change the signature to take `rows: Range<usize>, cols: Range<usize>`; iterate `plan.columns[cols.clone()]` for both the header and the fields (clamp `cols.end` to `plan.columns.len()`). Update the existing test call to pass `0..plan.columns.len()` (unchanged output) and add:

```rust
    #[test]
    fn a_block_yanks_only_its_columns_with_their_header() {
        // same fixture as tsv_has_a_header_indented_tree_text_raw_numbers_and_blanks
        let (snap, plan, visible) = fixture();
        let out = tsv(&snap, &plan, &visible, 1..3, 1..2);
        let header = &plan.columns[1].label;
        assert_eq!(out, format!("{header}\n1.5\n\n"));
    }
```
(Adjust `fixture()` extraction from the existing test so both tests share it; expected values follow that fixture: row 1 `delta01 = 1.5`, row 2 NULL.)

- [ ] **Step 4: Write the failing `summarize` tests** in `select.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::attribution::{Attribution, ScopeSemantics};
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::grid::selection::{SelectKind, resolve};
    use geode_core::snapshot::{ColumnMeta, Snapshot, TestColumn};

    // Root 9; L1 5 (child SPX 5); L2 4. `det` is DeterminedNonAdditive at depth 1.
    fn fixture() -> (Snapshot, ColumnPlan, Vec<u32>) {
        let meta = |n: &str, a: Vec<Attribution>| ColumnMeta {
            name: n.into(), attribution_by_depth: a, scope_semantics: ScopeSemantics::Direct,
        };
        let add = || vec![Attribution::Additive; 3];
        let snap = Snapshot::for_tests(vec![
            (meta("lhu", add()), TestColumn::Dict(vec![None, Some("L1".into()), Some("L2".into()), Some("L1".into())])),
            (meta("underlying_ref", add()), TestColumn::Dict(vec![None, None, None, Some("SPX".into())])),
            (meta("row_depth", add()), TestColumn::I32(vec![0, 1, 1, 2])),
            (meta("delta01", add()), TestColumn::F64(vec![Some(9.0), Some(5.0), Some(4.0), Some(5.0)])),
            (meta("det", vec![Attribution::Additive, Attribution::DeterminedNonAdditive, Attribution::DeterminedNonAdditive]),
             TestColumn::F64(vec![Some(1.0), Some(2.0), Some(2.0), Some(2.0)])),
        ], 2);
        let text = "[t]\ndataset = \"d\"\ngrouping = [\"lhu\", \"underlying_ref\"]\n\
            [[t.columns]]\nname = \"delta01\"\n[[t.columns]]\nname = \"det\"\n";
        let doc = merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()]);
        let view = geode_core::view::ViewSpec::from_doc(&doc).0.remove(0);
        let plan = ColumnPlan::build(&view, snap.grouping(), &snap);
        // Fully expanded flatten: root, L1, SPX, L2.
        (snap, plan, vec![0, 1, 3, 2])
    }

    #[test]
    fn a_group_with_its_child_sums_the_group_once() {
        let (snap, plan, shown) = fixture();
        let delta = plan.position_of("delta01").unwrap();
        // L1 and SPX selected (display 1..3).
        let r = resolve(SelectKind::Rows, (1, 0), (2, 0), plan.columns.len());
        let s = summarize(&snap, &plan, &shown, &r);
        let (_, text) = s.iter().find(|(l, _)| l == &plan.columns[delta].label).unwrap();
        assert!(text.starts_with("Σ 5.00 "), "{text}");
    }

    #[test]
    fn a_determined_non_additive_column_is_never_totalled() {
        let (snap, plan, shown) = fixture();
        let det = plan.position_of("det").unwrap();
        // L1 and L2 at depth 1 (display rows 1 and 3, with SPX between).
        let r = resolve(SelectKind::Rows, (1, 0), (3, 0), plan.columns.len());
        let s = summarize(&snap, &plan, &shown, &r);
        let (_, text) = s.iter().find(|(l, _)| l == &plan.columns[det].label).unwrap();
        assert!(text.starts_with("Σ —†"), "{text}");
    }

    #[test]
    fn a_block_over_one_measure_adds_extremes_and_skips_text_columns() {
        let (snap, plan, shown) = fixture();
        let delta = plan.position_of("delta01").unwrap();
        let r = resolve(SelectKind::Block, (1, 0), (3, delta), plan.columns.len());
        let s = summarize(&snap, &plan, &shown, &r);
        assert_eq!(s.len(), 1, "the tree column is not numeric: {s:?}");
        assert!(s[0].1.contains("min") && s[0].1.contains("max"), "{:?}", s[0]);
    }
}
```

The view text follows `yank.rs`'s `tsv_has_a_header_indented_tree_text_raw_numbers_and_blanks`. With no `label`/`format`, a column's label is its name and its format `ColumnFormat::MEASURE` — if `ColumnPlan::build` labels differently, match the fixture's lookups to `plan.columns[i].label`, which the tests already do.

- [ ] **Step 5: Implement `summarize`**

```rust
//! The blotter's selection summary (grid selection spec §3.3, §4.3):
//! per selected measure column, the aggregate over the selection's
//! top-most rows only — a group row already carries its children's
//! total — with non-additive cells refusing a sum.

use crate::core::plan::{ColumnKind, ColumnPlan};
use geode_core::attribution::Attribution;
use geode_core::grid::selection::{Accumulator, Resolved, describe, top_most};
use geode_core::snapshot::Snapshot;

pub fn summarize(
    snapshot: &Snapshot,
    plan: &ColumnPlan,
    shown: &[u32],
    resolved: &Resolved,
) -> Vec<(String, String)> {
    let end = resolved.rows.end.min(shown.len());
    let start = resolved.rows.start.min(end);
    let rows: Vec<usize> = shown[start..end].iter().map(|&r| r as usize).collect();
    let tree = snapshot.tree();
    let rows = top_most(&rows, snapshot.rows(), |r| tree.parent(r));
    let measures: Vec<usize> = resolved
        .cols
        .clone()
        .filter(|&c| plan.columns.get(c).is_some_and(|p| p.kind == ColumnKind::Measure))
        .collect();
    let extremes = measures.len() == 1;
    measures
        .into_iter()
        .map(|c| {
            let column = &plan.columns[c];
            let mut acc = Accumulator::default();
            if let Some(idx) = column.index {
                for &r in &rows {
                    let additive = plan.attribution(c, tree.depth(r)) == Attribution::Additive;
                    acc.add(snapshot.f64_at(idx, r), additive);
                }
            }
            (column.label.clone(), describe(&acc.finish(), &column.format, extremes))
        })
        .collect()
}
```

- [ ] **Step 6: Run the core tests**

Run: `cargo test -p geode-blotter --lib core::`
Expected: all `core::` tests pass (see Step 2 if the lib does not compile yet).

- [ ] **Step 7: Commit**

```bash
git add crates/geode-blotter/src/core
git commit -m "feat(blotter): exact path lookup, block TSV, selection summary

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: Blotter selection state, keys, tint, footer

**Files:**
- Modify: `crates/geode-blotter/src/delegate.rs`, `crates/geode-blotter/src/tile.rs`, `crates/geode-blotter/src/content.rs`

**Interfaces:**
- Consumes: Tasks 1–4.
- Produces (delegate, `pub`):
  ```rust
  pub selection: Option<Selection<Path, String>>,
  pub resolved: Option<Resolved>,
  pub summary: Vec<AggregateCell>,
  pub selection_lost: bool,            // set by refresh_selection, taken by the tile
  anchor_hint: usize,                  // private: last resolved anchor index, `find_by_path`'s `near`
  pub fn refresh_selection(&mut self)
  pub fn start_selection(&mut self, kind: SelectKind)
  pub fn clear_selection(&mut self)
  ```
  Actions: `blotter::visual_rows` ("Select rows"), `blotter::visual_block` ("Select cells") replace `blotter::visual`.

- [ ] **Step 1: Write the failing tile tests** (in `tile.rs` tests; they use the existing `open`, `next_query`, `deliver`, `snapshot()` helpers — the fixture's shown rows after `open` are `[root, L1, L2]` and after expanding L1 `[root, L1, SPX, L2]`; columns `[tree, delta01, daily_trading_pnl]`):

```rust
    fn act(h: &Harness, cx: &mut gpui::VisualTestContext, id: &str) -> bool {
        h.tile.update(cx, |t, cx| t.dispatch(&ActionId(id.into()), None, cx))
    }
    fn clip(cx: &mut gpui::VisualTestContext) -> Option<String> {
        cx.update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()))
    }
    fn delivered(cx: &mut gpui::TestAppContext) -> (Harness, gpui::VisualTestContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(snapshot()));
        (h, cx)
    }

    #[gpui::test]
    fn shift_v_selects_rows_and_y_copies_them_with_every_column(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = delivered(cx);
        act(&h, &mut cx, "blotter::visual_rows");
        act(&h, &mut cx, "blotter::down");
        act(&h, &mut cx, "blotter::yank");
        assert_eq!(clip(&mut cx).as_deref(),
            Some("lhu / underlying_ref\tdelta01\tdaily_trading_pnl\n\t9\t7\n  L1\t5\t7\n"));
        assert!(h.tile.read_with(&cx, |t, cx| t.table().read(cx).delegate().selection.is_none()),
            "yank ends the selection");
    }

    #[gpui::test]
    fn v_selects_a_block_and_y_copies_only_the_block(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = delivered(cx);
        act(&h, &mut cx, "blotter::right");            // cursor (0, delta01)
        act(&h, &mut cx, "blotter::visual_block");
        act(&h, &mut cx, "blotter::down");
        act(&h, &mut cx, "blotter::right");            // block rows 0..2 × cols 1..3
        act(&h, &mut cx, "blotter::yank");
        assert_eq!(clip(&mut cx).as_deref(), Some("delta01\tdaily_trading_pnl\n9\t7\n5\t7\n"));
    }

    #[gpui::test]
    fn the_other_key_switches_kind_and_the_same_key_clears(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = delivered(cx);
        let kind = |cx: &mut gpui::VisualTestContext| h.tile.read_with(cx, |t, cx|
            t.table().read(cx).delegate().selection.as_ref().map(|s| s.kind));
        act(&h, &mut cx, "blotter::visual_rows");
        act(&h, &mut cx, "blotter::down");
        act(&h, &mut cx, "blotter::visual_block");
        assert_eq!(kind(&mut cx), Some(SelectKind::Block));
        let rows = h.tile.read_with(&cx, |t, cx| t.table().read(cx).delegate().resolved.clone().unwrap().rows);
        assert_eq!(rows, 0..2, "the anchor survived the switch");
        act(&h, &mut cx, "blotter::visual_block");
        assert_eq!(kind(&mut cx), None);
    }

    #[gpui::test]
    fn escape_clears_the_selection_before_find(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = delivered(cx);
        h.tile.update(&mut cx, |t, cx| t.table().update(cx, |t, _| t.delegate_mut().set_narrowed(Some(vec![1, 2]))));
        act(&h, &mut cx, "blotter::visual_rows");
        act(&h, &mut cx, "blotter::escape");
        let (sel, narrowed) = h.tile.read_with(&cx, |t, cx| {
            let d = t.table().read(cx).delegate();
            (d.selection.is_some(), d.narrowed.is_some())
        });
        assert_eq!((sel, narrowed), (false, true), "first escape: selection only");
        act(&h, &mut cx, "blotter::escape");
        assert!(h.tile.read_with(&cx, |t, cx| t.table().read(cx).delegate().narrowed.is_none()));
    }

    #[gpui::test]
    fn the_footer_sums_a_group_and_its_child_once(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = delivered(cx);
        act(&h, &mut cx, "blotter::down");             // L1
        act(&h, &mut cx, "blotter::expand");           // shown: root, L1, SPX, L2
        act(&h, &mut cx, "blotter::visual_rows");
        act(&h, &mut cx, "blotter::down");             // L1 + SPX
        let summary = h.tile.read_with(&cx, |t, cx| t.table().read(cx).delegate().summary.clone());
        let delta = summary.iter().find(|c| c.label.as_ref() == "delta01").expect("delta01 summarised");
        assert!(delta.text.starts_with("Σ 5.00"), "{}", delta.text);
    }

    #[gpui::test]
    fn a_redelivery_keeps_the_selection_on_the_same_rows(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = delivered(cx);
        act(&h, &mut cx, "blotter::down");             // L1
        act(&h, &mut cx, "blotter::visual_rows");
        act(&h, &mut cx, "blotter::down");             // L1..L2 = rows 1..3
        // expand_all reflattens at once (SPX is materialised, so it lands
        // between L1 and L2) and always requeries.
        act(&h, &mut cx, "blotter::expand_all");
        let before = h.tile.read_with(&cx, |t, cx| t.table().read(cx).delegate().resolved.clone());
        assert_eq!(before.as_ref().map(|r| r.rows.clone()), Some(1..4), "anchor L1, cursor L2, by path");
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(snapshot()));
        let after = h.tile.read_with(&cx, |t, cx| t.table().read(cx).delegate().resolved.clone());
        assert_eq!(before, after, "a redelivery keeps the selection on the same rows");
    }

    #[gpui::test]
    fn a_selection_whose_anchor_row_vanishes_clears_with_a_notice(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = delivered(cx);
        act(&h, &mut cx, "blotter::bottom");           // L2
        act(&h, &mut cx, "blotter::visual_rows");
        // Narrow to rows that exclude L2.
        h.tile.update(&mut cx, |t, cx| {
            t.table().update(cx, |t, _| t.delegate_mut().set_narrowed(Some(vec![0, 1])));
            t.dispatch(&ActionId("blotter::up".into()), None, cx);
        });
        let (sel, err) = h.tile.read_with(&cx, |t, cx| (
            t.table().read(cx).delegate().selection.is_some(),
            t.error_text(),
        ));
        assert!(!sel);
        assert_eq!(err.as_deref(), Some("selection cleared: its first row is no longer shown"));
    }
```

Notes for the implementer:
- `t.error_text()`: if no accessor exists for `self.error`, add `#[cfg(test)] pub fn error_text(&self) -> Option<String>` returning `self.error.as_ref().map(|(e, _)| e.clone())` (check the field's type first).

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p geode-blotter --lib tile::`
Expected: compile errors (missing actions/fields).

- [ ] **Step 3: Delegate state.** In `delegate.rs`:
  - Replace `pub mode: Mode` with the fields listed under **Interfaces**; initialise `selection: None, resolved: None, summary: Vec::new(), selection_lost: false, anchor_hint: 0` in `new()`.
  - Add:

```rust
    /// Start a selection of `kind` at the cursor, or — when one is live —
    /// switch its kind keeping the anchor; the same kind again clears
    /// (spec §4.1).
    pub fn start_selection(&mut self, kind: SelectKind) {
        match self.selection.as_ref().map(|s| s.kind) {
            Some(k) if k == kind => self.selection = None,
            Some(_) => {
                if let Some(s) = self.selection.as_mut() {
                    s.kind = kind;
                }
            }
            None => {
                let (Some(path), Some(col)) = (
                    self.cursor_path(),
                    self.plan.as_ref().and_then(|p| p.columns.get(self.cursor.col)).map(|c| c.name.clone()),
                ) else {
                    return;
                };
                self.anchor_hint = self.cursor.row;
                self.selection = Some(Selection { kind, anchor_row: path, anchor_col: col });
            }
        }
        self.refresh_selection();
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
        self.refresh_selection();
    }

    /// Re-resolve the selection against the current rows and columns and
    /// rebuild the summary — every change point calls this, so render
    /// only ever looks `resolved` and `summary` up. An anchor no longer
    /// shown clears the selection and raises `selection_lost` for the
    /// tile's notice; no neighbouring row is guessed.
    pub fn refresh_selection(&mut self) {
        let resolved = match (&self.selection, &self.snapshot, &self.plan) {
            (Some(sel), Some(snapshot), Some(plan)) => {
                let hint = self.anchor_hint;
                let shown = &self.shown;
                sel.resolve_with(
                    (self.cursor.row, self.cursor.col),
                    plan.columns.len(),
                    |path| find_by_path(shown, snapshot, plan, path, hint),
                    |name| plan.position_of(name),
                )
            }
            _ => None,
        };
        if self.selection.is_some() && resolved.is_none() {
            self.selection = None;
            self.selection_lost = true;
        }
        if let Some(r) = &resolved {
            // `resolve` orders the range, so the anchor is whichever end
            // the cursor is not on.
            self.anchor_hint = if r.rows.start == self.cursor.row { r.rows.end - 1 } else { r.rows.start };
        }
        self.summary = match (&resolved, &self.snapshot, &self.plan) {
            (Some(r), Some(snapshot), Some(plan)) => summarize(snapshot, plan, &self.shown, r)
                .into_iter()
                .map(|(label, text)| AggregateCell { label: label.into(), text: text.into() })
                .collect(),
            _ => Vec::new(),
        };
        self.resolved = resolved;
    }
```
  - Call `self.refresh_selection()` as the last statement of `reflatten_keeping` (both the early-return branch and the normal path — in the early return, after clearing `visible`/`shown`), `set_narrowed`, and `move_column` (before `cx.notify()`).

- [ ] **Step 4: Tint.** Replace `render_tr`'s body:

```rust
        let tint = self
            .resolved
            .as_ref()
            .is_some_and(|r| r.kind == SelectKind::Rows && r.contains_row(row_ix));
        div()
            .id(("row", row_ix))
            .when(tint, |el| el.bg(cx.theme().selection.opacity(0.35)))
```
In `render_td`, after the `is_cursor` computation add
`let in_block = self.resolved.as_ref().is_some_and(|r| r.kind == SelectKind::Block && r.contains(row_ix, col_ix));`
and chain `.when(in_block, |el| el.bg(theme.selection.opacity(0.35)))` directly before `.when(is_cursor, ...)`.

- [ ] **Step 5: Tile.** In `tile.rs`:
  - `ACTIONS`: replace `("blotter::visual", "Visual mode")` with `("blotter::visual_rows", "Select rows")` and `("blotter::visual_block", "Select cells")`.
  - `key_context`:
    ```rust
    let d = self.table.read(cx).delegate();
    let mut ctx = KeyContext::new("blotter")
        .pair("mode", if d.selection.is_some() { "visual" } else { "normal" });
    if let Some(s) = &d.selection {
        ctx = ctx.pair("select", match s.kind { SelectKind::Rows => "rows", SelectKind::Block => "block" });
    }
    ctx.counts()
    ```
    (check `KeyContext::pair`'s signature — if it borrows `&'static str`, this compiles as written.)
  - Motions: `let wrap = d.selection.is_none();`
  - `sync_cursor`: first `self.with_delegate(cx, |d| d.refresh_selection());`, then the existing body, then `self.take_selection_notice(cx); cx.notify();`.
  - Replace the `"visual"` arm with:
    ```rust
            "visual_rows" | "visual_block" => {
                let kind = if name == "visual_rows" { SelectKind::Rows } else { SelectKind::Block };
                self.with_delegate(cx, |d| d.start_selection(kind));
                self.table.update(cx, |t, cx| cx.notify());
                cx.notify();
            }
    ```
    (This also fixes the old arm's missing repaint.)
  - `"escape"`: prepend a guard to the existing arm body (the mutation entry anchors on its first line):
    ```rust
            "escape" => {
                if self.with_delegate(cx, |d| d.selection.is_some()) {
                    self.with_delegate(cx, |d| d.clear_selection());
                    cx.notify();
                    return true;
                }
                // ... the existing body unchanged ...
            }
    ```
    (If `dispatch` has shared tail work after the `match` — e.g. the `take_selection_notice` call you add below — use an `else` block instead of `return true` so the tail still runs.)
  - `"yank"`:
    ```rust
            "yank" => {
                let text = self.with_delegate(cx, |d| {
                    let (Some(snapshot), Some(plan)) = (&d.snapshot, &d.plan) else { return None };
                    let (rows, cols) = match &d.resolved {
                        Some(r) => (r.rows.clone(), r.cols.clone()),
                        None => (d.cursor.row..d.cursor.row + 1, 0..plan.columns.len()),
                    };
                    let out = tsv(snapshot, plan, &d.shown, rows, cols);
                    d.clear_selection();
                    Some(out)
                });
                if let Some(text) = text {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
                cx.notify();
            }
    ```
  - Add
    ```rust
    fn take_selection_notice(&mut self, cx: &mut Context<Self>) {
        if self.with_delegate(cx, |d| std::mem::take(&mut d.selection_lost)) {
            self.error = Some(("selection cleared: its first row is no longer shown".into(), Tone::WarningText));
        }
    }
    ```
    and call it after `apply_snapshot` in the delivery path (beside the `dropped_sort` handling) and at the end of `dispatch` for every handled action.
  - Footer: after the `{} rows` child, `.when(!delegate.summary.is_empty(), |f| f.child(aggregates::strip(&delegate.summary, theme)))`; when `delegate.summary.is_empty()` and `delegate.resolved.is_some()`, add a child with `format!("{} × {} selected", r.rows.len(), r.cols.len())`. Since the footer uses `gap_4` and `text_xs`, the strip inherits the size. Show the existing `†` legend when any summary text contains `†` as well as when `any_determined`.
  - Imports: remove `crate::core::cursor::{Mode, selection}`; add `geode_core::grid::selection::SelectKind`, `geode_shell::shell::aggregates`.

- [ ] **Step 6: Keymap.** In `content.rs` normal context replace `"v" = "blotter::visual"` with
  ```toml
  "v" = "blotter::visual_block"
  "shift+v" = "blotter::visual_rows"
  ```
  In the visual context: add `h`, `l`, `home`, `end`, `^`, `$` bound as in normal; replace `"v" = "blotter::escape"` with `"v" = "blotter::visual_block"` and add `"shift+v" = "blotter::visual_rows"`. Update the module doc above `DEFAULT_KEYMAP`: visual is "motions (both axes), `y`, `v`/`V` to switch or leave, and `escape`".
  Add a content.rs test beside `caret_and_dollar_resolve_to_the_column_extremes`, same setup:
  ```rust
    #[test]
    fn v_and_shift_v_start_the_two_selections_and_h_moves_in_visual() {
        // ... same keymap/registry setup as caret_and_dollar_resolve_to_the_column_extremes ...
        let normal = [KeyContext::new("workspace"), KeyContext::new("tile"),
                      KeyContext::new("blotter").pair("mode", "normal").counts()];
        let visual = [KeyContext::new("workspace"), KeyContext::new("tile"),
                      KeyContext::new("blotter").pair("mode", "visual").counts()];
        for (stack, spec, expected) in [
            (&normal, "v", "blotter::visual_block"),
            (&normal, "shift+v", "blotter::visual_rows"),
            (&visual, "v", "blotter::visual_block"),
            (&visual, "shift+v", "blotter::visual_rows"),
            (&visual, "h", "blotter::left"),
            (&visual, "l", "blotter::right"),
        ] {
            let keystroke = parse_keystroke(spec, default_mod()).unwrap();
            match Matcher::default().press(&keymap, keystroke, stack) {
                MatchResult::Matched { action, .. } => assert_eq!(action.0, expected, "{spec}"),
                other => panic!("{spec}: expected a match, got {other:?}"),
            }
        }
    }
  ```

- [ ] **Step 7: Update the old tests.** In `motions_expansion_and_yank`, `blotter::visual` → `blotter::visual_rows` and the `Mode::Normal` assertion → `delegate().selection.is_none()`. In `a_bare_j_wraps_in_normal_mode_and_clamps_in_visual` (around line 2681): `blotter::visual` → `blotter::visual_rows`; the `Mode::Visual { anchor: 2 }` assertion → `delegate().resolved.as_ref().map(|r| r.rows.clone())` equal to the range from the anchor (row 2) to the clamped cursor — read the test and write the equivalent range.

- [ ] **Step 8: Run tests**

Run: `cargo test -p geode-blotter`
Expected: all pass, including the 7 new tile tests and the keymap test.

- [ ] **Step 9: Mutation entries** (section `# ---- blotter selection (grid selection spec)`):

```zsh
run_mutation "blotter selection: escape clears the selection before find" \
  crates/geode-blotter/src/tile.rs \
  '                if self.with_delegate(cx, |d| d.selection.is_some()) {' \
  '                if false {' \
  geode-blotter \
  escape_clears_the_selection_before_find

run_mutation "blotter selection: a lost anchor clears and raises the notice" \
  crates/geode-blotter/src/delegate.rs \
  '            self.selection_lost = true;' \
  '            self.selection_lost = false;' \
  geode-blotter \
  a_selection_whose_anchor_row_vanishes_clears_with_a_notice

run_mutation "blotter selection: summarize goes through top_most" \
  crates/geode-blotter/src/core/select.rs \
  '    let rows = top_most(&rows, snapshot.rows(), |r| tree.parent(r));' \
  '    let _ = top_most(&rows, snapshot.rows(), |r| tree.parent(r));' \
  geode-blotter \
  a_group_with_its_child_sums_the_group_once
```
Make sure each anchor string matches the code you wrote exactly once (write the escape arm's guard as `if self.with_delegate(cx, |d| d.selection.is_some()) {`). Run `--anchors-only "blotter selection"`, commit, then `zsh scripts/mutation-check.sh "blotter selection"` → all `caught`.

- [ ] **Step 10: Commit**

```bash
git add crates/geode-blotter scripts/mutation-check.sh
git commit -m "feat(blotter): V selects rows, v selects a block; footer aggregates

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: Blotter mouse — shift+click and drag

**Files:**
- Modify: `crates/geode-blotter/src/delegate.rs`, `crates/geode-blotter/src/tile.rs`

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum CellPointer {
      Press { row: usize, col: usize, shift: bool, gutter: bool },
      Drag { row: usize, col: usize, gutter: bool },
  }
  impl EventEmitter<CellPointer> for TableState<BlotterDelegate> {}
  ```
  Delegate private field `drag_last: Option<(usize, usize)>`.

Rules (order-independent by construction; see Deviation 2):
- Handlers never call `cx.stop_propagation()`.
- The table's `SelectRow` handler keeps only moving the cursor row (then `sync_cursor`), never clearing a selection.
- `Press { shift: false }` clears the selection, moves the cursor to `(row, col)`.
- `Press { shift: true }` with no selection starts `Rows` if `gutter` else `Block` at the *current* cursor, then moves the cursor to `(row, col)`; with a selection it only moves the cursor.
- `Drag`: ignored when `(row, col)` is the cursor; otherwise, with no selection, start (`Rows` if `gutter` else `Block`) at the cursor; then move the cursor.
- The gutter is a child of the tree cell, so a gutter press reaches the cell handler too: the gutter event arrives first and the cell's repeat is harmless under these rules.

- [ ] **Step 1: Write the failing tests** (tile.rs tests; the cell selector `blotter-cell-{row}-{col}` already exists; `gpui::{Modifiers, MouseButton}`):

```rust
    fn centre(cx: &mut gpui::VisualTestContext, sel: &str) -> gpui::Point<gpui::Pixels> {
        cx.run_until_parked();
        cx.debug_bounds(sel).unwrap_or_else(|| panic!("{sel} not painted")).center()
    }

    #[gpui::test]
    fn shift_click_extends_a_block_from_the_cursor(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = delivered(cx);
        act(&h, &mut cx, "blotter::right");                          // (0, 1)
        let at = centre(&mut cx, "blotter-cell-2-2");
        cx.simulate_mouse_down(at, MouseButton::Left, Modifiers::shift());
        cx.simulate_mouse_up(at, MouseButton::Left, Modifiers::shift());
        let r = h.tile.read_with(&cx, |t, cx| t.table().read(cx).delegate().resolved.clone()).unwrap();
        assert_eq!((r.kind, r.rows, r.cols), (SelectKind::Block, 0..3, 1..3));
        // The keyboard keeps extending what the mouse started.
        act(&h, &mut cx, "blotter::up");
        let r = h.tile.read_with(&cx, |t, cx| t.table().read(cx).delegate().resolved.clone()).unwrap();
        assert_eq!(r.rows, 0..2);
    }

    #[gpui::test]
    fn a_drag_selects_a_block_and_a_plain_click_clears_it(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = delivered(cx);
        let from = centre(&mut cx, "blotter-cell-0-1");
        let to = centre(&mut cx, "blotter-cell-1-2");
        cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(to, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::none());
        let r = h.tile.read_with(&cx, |t, cx| t.table().read(cx).delegate().resolved.clone()).unwrap();
        assert_eq!((r.rows, r.cols), (0..2, 1..3));
        let elsewhere = centre(&mut cx, "blotter-cell-2-1");
        cx.simulate_mouse_down(elsewhere, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(elsewhere, MouseButton::Left, Modifiers::none());
        let (sel, cursor) = h.tile.read_with(&cx, |t, cx| {
            let d = t.table().read(cx).delegate();
            (d.selection.is_some(), (d.cursor.row, d.cursor.col))
        });
        assert_eq!((sel, cursor), (false, (2, 1)));
    }
```
If `simulate_mouse_move`'s button parameter is `Option<MouseButton>` in the pinned gpui, pass `Some(MouseButton::Left)` (check `VisualTestContext` in the registry source).

Add a gutter test only if the harness can enable line numbers (`delegate.line_numbers = LineNumbers::On` then refresh); if so: shift+click on `blotter-gutter-2` from cursor row 0 → `resolved.kind == Rows`, `rows == 0..3`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p geode-blotter --lib shift_click_extends_a_block_from_the_cursor a_drag_selects`
Expected: FAIL (no selection).

- [ ] **Step 3: Emit from the delegate.** In `render_td`, on the root `el` (after `.debug_selector`), add:

```rust
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                this.delegate_mut().drag_last = Some((row_ix, col_ix));
                cx.emit(CellPointer::Press { row: row_ix, col: col_ix, shift: e.modifiers.shift, gutter: false });
            }))
            .on_mouse_move(cx.listener(move |this, e: &MouseMoveEvent, _, cx| {
                if e.pressed_button != Some(MouseButton::Left) {
                    return;
                }
                let d = this.delegate_mut();
                if d.drag_last == Some((row_ix, col_ix)) {
                    return;
                }
                d.drag_last = Some((row_ix, col_ix));
                cx.emit(CellPointer::Drag { row: row_ix, col: col_ix, gutter: false });
            }))
```
Add the same two handlers to the gutter `div` with `gutter: true`. Each listener captures two `usize`s (same cost class as the chevron's).

- [ ] **Step 4: Handle in the tile.** Subscribe beside the `ChevronClicked` subscription:

```rust
        cx.subscribe(&table, |this, _, event: &CellPointer, cx| this.pointer(*event, cx))
            .detach();
```
and add:
```rust
    /// Every mouse selection gesture (spec §5) lands here and goes
    /// through the same `start_selection`/`clear_selection` doors the
    /// keys use.
    fn pointer(&mut self, event: CellPointer, cx: &mut Context<Self>) {
        let kind_for = |gutter: bool| if gutter { SelectKind::Rows } else { SelectKind::Block };
        self.with_delegate(cx, |d| {
            let (row, col, start) = match event {
                CellPointer::Press { row, col, shift: false, .. } => {
                    d.selection = None;
                    (row, col, None)
                }
                CellPointer::Press { row, col, shift: true, gutter } => (row, col, Some(kind_for(gutter))),
                CellPointer::Drag { row, col, gutter } => {
                    if (row, col) == (d.cursor.row, d.cursor.col) {
                        return;
                    }
                    (row, col, Some(kind_for(gutter)))
                }
            };
            if let Some(kind) = start
                && d.selection.is_none()
            {
                d.start_selection(kind);
            }
            let cols = d.plan.as_ref().map_or(0, |p| p.columns.len());
            d.cursor.to_row(row, d.shown.len());
            d.cursor.col = col.min(cols.saturating_sub(1));
        });
        self.sync_cursor(cx);
    }
```
(`sync_cursor` re-resolves, takes the notice and notifies — Task 5 Step 5.)

- [ ] **Step 5: Run tests**

Run: `cargo test -p geode-blotter`
Expected: all pass.

- [ ] **Step 6: Mutation entry**

```zsh
run_mutation "blotter selection: a shift press starts a selection" \
  crates/geode-blotter/src/tile.rs \
  '                CellPointer::Press { row, col, shift: true, gutter } => (row, col, Some(kind_for(gutter))),' \
  '                CellPointer::Press { row, col, shift: true, .. } => (row, col, None),' \
  geode-blotter \
  shift_click_extends_a_block_from_the_cursor
```
`--anchors-only`, commit, run the entry → `caught`.

- [ ] **Step 7: Commit**

```bash
git add crates/geode-blotter scripts/mutation-check.sh
git commit -m "feat(blotter): shift+click and drag select

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7: Bench, docs, gate

**Files:**
- Modify: `crates/geode-blotter/benches/blotter.rs`, `docs/current/performance.md`, `docs/current/features.md`, `docs/current/keymaps.md`, `crates/geode-blotter/README.md`, `crates/geode-core/README.md`

- [ ] **Step 1: Bench.** In `benches/blotter.rs`, inside `bench`, using the existing 720,881-row `shape(..)` and its fully expanded flatten (copy how the file builds `plan` and the expanded `visible` for the flatten bench):

```rust
    {
        use geode_blotter::core::select::summarize;
        use geode_core::grid::selection::{SelectKind, resolve};
        // `V` then `G` from the top: every row, every measure column.
        let r = resolve(SelectKind::Rows, (0, 0), (visible.len() - 1, 0), plan.columns.len());
        c.bench_function("summarize/720881 rows, all columns", |b| {
            b.iter(|| black_box(summarize(&snapshot, &plan, &visible, &r)))
        });
    }
```
Update the file's module doc to list the new bench. Run: `cargo bench -p geode-blotter --bench blotter -- summarize` (release build; run detached if it exceeds a few minutes per the sccache memory). Record the median in `docs/current/performance.md` beside the blotter core rows with machine + date.
If the median exceeds 8 ms, **stop and report** to the controller with the number — do not optimise without a ruling (the candidate is summarising only the top-most rows' measure columns lazily, which changes Task 4's shape).

- [ ] **Step 2: Docs.**
  - `docs/current/features.md`, blotter section: a "Selection" paragraph — `V` rows / `v` block, switching and clearing, motions extend and clamp, `y` TSV (rows: all columns; block: its columns with their header), footer aggregates (top-most rows only; `Σ —†` for non-additive columns), shift+click/drag, anchor loss notice. Limitations: contiguous only, no paste.
  - `docs/current/keymaps.md`: in the context-examples section, note `mode == visual` and the `select == rows|block` pair tiles push while a selection is live.
  - `crates/geode-blotter/README.md`: module map gets `core::select`; the yank line becomes "yank as TSV (rows or a cell block)".
  - `crates/geode-core/README.md`: module map gets `grid::selection`.

- [ ] **Step 3: Gate.**

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo check -p geode-shell --features test-support --all-targets
zsh scripts/mutation-check.sh --anchors-only
```
Expected: all clean; `--anchors-only` exit 0.

- [ ] **Step 4: Commit**

```bash
git add -A crates/geode-blotter/benches docs crates/geode-blotter/README.md crates/geode-core/README.md
git commit -m "docs(blotter): selection behaviour, keys, bench

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

- [ ] **Step 5: Display check (Matthew's eyes).** Record in the handoff: `cargo run -p geode-app -- --demo`, on a light and a dark theme — `V` tint across full rows; `v` block tint with the cursor border visible inside it; footer strip legible with `†` legend; shift+click and drag feel.
