# Pricer Package-Row Cells Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A package row's text columns show its legs' distinct values joined
with `/`, and qty shows the package quantity. Each of these cells edits:
- a single value goes to every leg;
- a `/` list maps by position onto the displayed distinct values;
- package qty rescales the legs by their weights.

**Architecture:** A pure `core::package` module groups a package's legs by
value, in leg order. Display spells each group, and a commit maps typed parts
onto the groups. Each leg's edit comes from the existing single-line
`cell::edit_for`, so the per-value rules (parsing, validation, barrier-only
legs, shift inherit) live in one place. `columns::cell_text` and
`cell::editor_for` route package rows to the module. The tile's commit path
takes a list of edits and applies several through the existing one-undo
`apply_edits`.

**Tech Stack:** Rust. `geode_pricer::core` (`Sheet`, `Edit`, `TemplateSet`,
`render_package`), GPUI for the tile tests.

**Spec:** `docs/superpowers/specs/2026-09-27-pricer-package-row-cells-design.md`

## Global Constraints

- **Text columns:** `underlying`, `expiry`, `strike`, `type`, `barrier`, `barrier type`, `spot shift`, `vol shift`. Each shows the distinct values in leg order joined with `/`, spelled as a line's cell spells them.
- **Distinctness:** values are compared as values, not text.
  - Barrier columns consider only barrier legs; with none, the cell is blank.
  - Shifts group by the effective value (own, else the sheet's). The cell is `CellState::Inherited` when every leg inherits, otherwise `Own`.
- **Qty:**
  - When `render_package(def, legs)` is `Some` for the package's template, qty shows `first leg qty / first leg weight` as the package quantity.
  - Otherwise qty shows the `/` list of distinct leg quantities.
- **Edit mapping:**
  - One part goes to every group.
  - A list of exactly `groups.len()` parts maps by position.
  - Any other count is refused with `"{n} values: {current}"`.
  - Package qty takes one non-zero integer `q`, and each leg becomes `q × weight` (checked). A list is refused with `"one quantity"`, and overflow with `"quantity out of range"`.
- **Commit:**
  - Every part is validated (through `edit_for`) before anything is applied.
  - The whole commit is one undo entry.
  - A commit that changes no leg is no edit.
- **Editor and nudge:**
  - Package rows open a plain text editor, even for expiry and type.
  - A value containing `/` does not nudge.
- **Rendering:** no formatting in render. Cells are prepared by `GridModel::build`, as today.
- **Docs:** `docs/current/features.md` and `crates/geode-pricer/README.md` change with the behavior.
- **Mutation harness:** commit before targeted runs; `zsh scripts/mutation-check.sh --anchors-only` exits 0 at each task end.
- **Commits** end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

1. A package whose template is unknown or doesn't fit shows its qty as a list, and its qty edits map by position, not by rescaling.
2. A list edit on a fly moves the body leg once. The body is one group, not two.
3. A barrier-type edit on a CUSTOM package that mixes barrier and vanilla legs touches only the barrier legs.
4. An invalid part anywhere in a list refuses the whole commit, leaving every leg unchanged.
5. Undo after a multi-leg commit restores every leg in one step.

Tests for 1–4 are in Tasks 1–2, and for 5 in Task 3.

---

### Task 1: Package display (`core::package::aggregate`)

**Files:**
- Create: `crates/geode-pricer/src/core/package.rs`
- Modify: `crates/geode-pricer/src/core/mod.rs` (`pub mod package;`)
- Modify: `crates/geode-pricer/src/core/columns.rs` (route package rows; make `own`, `blank`, `signed` reachable as `pub(crate)` if they aren't already)
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Produces:
  - `pub fn aggregates(kind: ColumnKind) -> bool`: true for Qty and the eight text columns.
  - `pub fn aggregate(sheet: &Sheet, row: usize, kind: ColumnKind, format: &ColumnFormat) -> CellText`
  - `pub(crate) fn package_qty(sheet: &Sheet, row: usize) -> Option<(i64, Vec<i64>)>`: the package quantity and the template weights, when the legs fit.
  - `pub(crate) enum Groups`: the grouping Task 2 consumes (below).

- [ ] **Step 1: Failing tests** (`package.rs` tests module)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::columns::column;
    use crate::core::edit::Edit;
    use crate::core::sheet::tests::{line, push, spx};
    use crate::core::shorthand::parse_builtin;
    use crate::core::template::Template;
    use geode_core::pricing::{Barrier, BarrierKind, Instrument, OptionKind};

    fn sheet_of(lines: &[&str]) -> Sheet {
        let mut s = Sheet::new("t");
        push(&mut s, lines.iter().map(|l| parse_builtin(l).unwrap()).collect());
        s
    }

    fn text(s: &Sheet, row: usize, name: &str) -> (String, CellState) {
        let def = column(name).unwrap();
        let c = aggregate(s, row, def.kind, &def.default_format);
        (c.text, c.state)
    }

    #[test]
    fn a_call_spread_shows_one_underlying_one_expiry_and_both_strikes() {
        let s = sheet_of(&["-5 SPX Z26 7400/7800 CS"]);
        assert_eq!(text(&s, 0, "underlying").0, "SPX");
        assert_eq!(text(&s, 0, "expiry").0, "Z26");
        assert_eq!(text(&s, 0, "strike").0, "7400/7800");
        assert_eq!(text(&s, 0, "type").0, "C");
        assert_eq!(text(&s, 0, "qty"), ("-5".into(), CellState::Own), "the package quantity");
        assert_eq!(text(&s, 0, "barrier").0, "", "no barrier leg");
    }

    #[test]
    fn distinct_values_keep_leg_order_and_collapse_repeats() {
        let s = sheet_of(&["SPX Z26 7600 STRD", "SPX Z26 7400/7600/7800 FLY", "SPX Z26/H27 7600 CAL", "SPX Z26 7400/7800 RR"]);
        let pkg = |i: usize| s.roots().nth(i).unwrap();
        assert_eq!(text(&s, pkg(0), "strike").0, "7600", "a straddle's one strike");
        assert_eq!(text(&s, pkg(0), "type").0, "C/P");
        assert_eq!(text(&s, pkg(1), "strike").0, "7400/7600/7800", "the fly's body once");
        assert_eq!(text(&s, pkg(2), "expiry").0, "H27/Z26", "leg order: CAL's first leg is the far expiry");
        assert_eq!(text(&s, pkg(3), "type").0, "P/C", "RR: short put leg first");
    }

    #[test]
    fn qty_falls_back_to_the_leg_list_when_the_legs_do_not_fit() {
        let mut s = sheet_of(&["-5 SPX Z26 7400/7800 CS"]);
        s.apply(Edit::SetQty { row: 2, qty: 3 }).unwrap(); // legs -5 / 3: no longer a CS
        assert_eq!(text(&s, 0, "qty").0, "-5/3");
        let mut c = Sheet::new("t");
        push(&mut c, vec![line(spx(5000.0, OptionKind::Call), 2)]);
        push(&mut c, vec![line(spx(4000.0, OptionKind::Put), 2)]);
        c.apply(Edit::Group { first: 0, count: 2, template: Template::CUSTOM, id: None }).unwrap();
        assert_eq!(text(&c, 0, "qty").0, "2", "a custom package's legs: one distinct quantity");
    }

    #[test]
    fn barrier_columns_read_only_barrier_legs() {
        let mut s = Sheet::new("t");
        let barrier = Instrument::Barrier(Barrier {
            vanilla: match spx(5000.0, OptionKind::Call) {
                Instrument::Vanilla(v) => v,
                _ => unreachable!(),
            },
            barrier: BarrierKind::UpOut,
            level: 5500.0,
        });
        push(&mut s, vec![line(barrier, 1)]);
        push(&mut s, vec![line(spx(4000.0, OptionKind::Put), 1)]);
        s.apply(Edit::Group { first: 0, count: 2, template: Template::CUSTOM, id: None }).unwrap();
        assert_eq!(text(&s, 0, "barrier").0, "5500");
        assert_eq!(text(&s, 0, "barrier_type").0, "UO");
    }

    #[test]
    fn shifts_group_by_effective_value_and_mute_when_all_inherit() {
        let mut s = sheet_of(&["SPX Z26 7400/7800 CS"]);
        assert_eq!(text(&s, 0, "spot_shift"), ("".into(), CellState::Blank), "nothing set anywhere");
        s.apply(Edit::SetSheetShift { shift: crate::core::sheet::OwnShifts { spot_pct: Some(2.0), vol_pts: None } }).unwrap();
        let (t, state) = text(&s, 0, "spot_shift");
        assert_eq!(state, CellState::Inherited, "every leg inherits");
        assert!(!t.contains('/'), "one effective value: {t}");
        s.apply(Edit::SetShift { row: 2, shift: crate::core::sheet::OwnShifts { spot_pct: Some(2.0), vol_pts: None } }).unwrap();
        let (t2, state2) = text(&s, 0, "spot_shift");
        assert_eq!(t2, t, "an own 2 and an inherited 2 are one value");
        assert_eq!(state2, CellState::Own, "one leg sets its own");
    }
}
```

Adapt these to the real API: `column`'s field names (`kind`, `default_format`), `Edit::SetSheetShift`'s shape, `Barrier`'s field names, the `CellState` variants (`Own`, `Inherited`, `Blank`) and `ColumnDef`'s names for the barrier-type column. Keep every expected string. The CAL order follows `BUILTIN_TEMPLATES`: `CAL` leg 1 is `expiry = 2` (far). Check it against the table, and if leg 1 is near, the expectation is `Z26/H27`.

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p geode-pricer core::package`. It fails to compile.

- [ ] **Step 3: Implement** (`package.rs` above the tests)

```rust
//! A package row's aggregated cells (package-row spec): its legs' distinct
//! values in leg order, joined with `/`, and the package quantity while
//! the legs fit the package's template. `commit` maps an edit back onto
//! the legs by the same grouping. Pure: the tile applies what it answers.

use crate::core::columns::{CellState, CellText, ColumnKind, signed};
use crate::core::sheet::{RowKind, Sheet};
use crate::core::shorthand::{render_barrier_kind, render_expiry, render_package, render_strike};
use geode_core::pricing::{Instrument, OptionKind, Strike};
use geode_core::view::ColumnFormat;

pub fn aggregates(kind: ColumnKind) -> bool {
    matches!(
        kind,
        ColumnKind::Qty
            | ColumnKind::Underlying
            | ColumnKind::Expiry
            | ColumnKind::Strike
            | ColumnKind::Type
            | ColumnKind::Barrier
            | ColumnKind::BarrierType
            | ColumnKind::SpotShift
            | ColumnKind::VolShift
    )
}

/// The legs of a package grouped by one column's value, groups in the
/// order their first leg appears. `display` is the cell's spelling of a
/// group; `edit` is the spelling the line editor would open on, so a list
/// typed back parses as the line cell parses it.
pub(crate) struct Group {
    pub display: String,
    pub edit: String,
    pub legs: Vec<usize>,
}

fn group_by<T: PartialEq>(
    legs: impl Iterator<Item = (usize, T)>,
    spell: impl Fn(&T) -> (String, String),
) -> Vec<Group> {
    let mut keys: Vec<T> = Vec::new();
    let mut out: Vec<Group> = Vec::new();
    for (leg, v) in legs {
        match keys.iter().position(|k| *k == v) {
            Some(i) => out[i].legs.push(leg),
            None => {
                let (display, edit) = spell(&v);
                out.push(Group { display, edit, legs: vec![leg] });
                keys.push(v);
            }
        }
    }
    out
}

/// `{}` of an `f64`, as the line editor spells a shift or a barrier level.
fn plain(v: f64) -> String {
    format!("{v}")
}

/// The package quantity and the template's weights, when the legs fit
/// the package's template (the shorthand's own test).
pub(crate) fn package_qty(sheet: &Sheet, row: usize) -> Option<(i64, Vec<i64>)> {
    let RowKind::Package { template } = sheet.kind(row) else {
        return None;
    };
    let def = sheet.templates().resolve(template.token())?;
    let legs: Vec<(i64, &Instrument)> = sheet
        .children(row)
        .filter_map(|l| sheet.instrument(l).map(|i| (sheet.qty(l), i)))
        .collect();
    render_package(def, &legs)?;
    let q = legs.first()?.0.checked_div(def.legs.first()?.weight)?;
    Some((q, def.legs.iter().map(|l| l.weight).collect()))
}

/// The groups of `row`'s legs for `kind`; empty for a column that does not
/// aggregate or a package with no leg the column reads.
pub(crate) fn groups(sheet: &Sheet, row: usize, kind: ColumnKind, format: &ColumnFormat) -> Vec<Group> {
    let legs = || sheet.children(row).filter_map(|l| sheet.instrument(l).map(|i| (l, i)));
    let barrier_legs = || {
        legs().filter_map(|(l, i)| match i {
            Instrument::Barrier(b) => Some((l, b)),
            Instrument::Vanilla(_) => None,
        })
    };
    let same = |s: String| (s.clone(), s);
    match kind {
        ColumnKind::Qty => group_by(sheet.children(row).map(|l| (l, sheet.qty(l))), |q| same(q.to_string())),
        ColumnKind::Underlying => group_by(legs().map(|(l, i)| (l, i.underlying().to_string())), |u| same(u.clone())),
        ColumnKind::Expiry => group_by(legs().map(|(l, i)| (l, i.expiry().clone())), |e| same(render_expiry(e))),
        ColumnKind::Strike => group_by(legs().map(|(l, i)| (l, i.strike())), |k| same(render_strike(*k))),
        ColumnKind::Type => group_by(legs().map(|(l, i)| (l, i.kind())), |k| {
            same(match k {
                OptionKind::Call => "C".into(),
                OptionKind::Put => "P".into(),
            })
        }),
        ColumnKind::Barrier => group_by(barrier_legs().map(|(l, b)| (l, b.level)), |v| {
            (render_strike(Strike::Absolute(*v)), plain(*v))
        }),
        ColumnKind::BarrierType => group_by(barrier_legs().map(|(l, b)| (l, b.barrier)), |k| {
            same(render_barrier_kind(*k).to_string())
        }),
        ColumnKind::SpotShift | ColumnKind::VolShift => {
            let pick = |s: crate::core::sheet::OwnShifts| match kind {
                ColumnKind::SpotShift => s.spot_pct,
                _ => s.vol_pts,
            };
            let sheet_value = pick(sheet.sheet_shift());
            group_by(
                sheet.children(row).map(|l| (l, pick(sheet.shift(l)).or(sheet_value))),
                |v| match v {
                    Some(v) => (signed(*v, format), plain(*v)),
                    None => (String::new(), String::new()),
                },
            )
        }
        _ => Vec::new(),
    }
}

/// A package row's cell for `kind` (see the module doc).
pub fn aggregate(sheet: &Sheet, row: usize, kind: ColumnKind, format: &ColumnFormat) -> CellText {
    if kind == ColumnKind::Qty
        && let Some((q, _)) = package_qty(sheet, row)
    {
        return CellText { text: q.to_string(), state: CellState::Own };
    }
    let gs = groups(sheet, row, kind, format);
    let text = gs.iter().map(|g| g.display.as_str()).collect::<Vec<_>>().join("/");
    if text.is_empty() {
        return CellText { text, state: CellState::Blank };
    }
    let state = match kind {
        ColumnKind::SpotShift | ColumnKind::VolShift => {
            let pick = |s: crate::core::sheet::OwnShifts| match kind {
                ColumnKind::SpotShift => s.spot_pct,
                _ => s.vol_pts,
            };
            if sheet.children(row).all(|l| pick(sheet.shift(l)).is_none()) {
                CellState::Inherited
            } else {
                CellState::Own
            }
        }
        _ => CellState::Own,
    };
    CellText { text, state }
}
```

Use the real names of `CellText`/`CellState` and of the `OwnShifts` path; if `if let … &&` chains are not used in this crate, write a nested `if let`. A group whose display is empty (a shift nobody sets) contributes an empty part. Only when every group is empty is the cell `Blank`, which the joined text covers.

In `columns::cell_text`, before the `applies` check, add:

```rust
    // A package row aggregates its legs' values (package-row spec).
    if sheet.is_package(row) && crate::core::package::aggregates(def.kind) {
        return crate::core::package::aggregate(sheet, row, def.kind, format);
    }
```

Update the existing test `instrument_cells_render_the_grammar_and_a_package_paints_them_blank`: rename it to `instrument_cells_render_the_grammar_and_a_package_aggregates_them`, and change its package assertions to the aggregated text.

- [ ] **Step 4: Run to see them pass**

Run: `cargo test -p geode-pricer`. All pass.

- [ ] **Step 5: Mutation entries**

```zsh
# A package's cells aggregate its legs; without the route they paint blank.
run_mutation "pricer package: text cells paint blank" \
  crates/geode-pricer/src/core/columns.rs \
  '    if sheet.is_package(row) && crate::core::package::aggregates(def.kind) {' \
  '    if false && crate::core::package::aggregates(def.kind) {' \
  geode-pricer a_call_spread_shows_one_underlying_one_expiry_and_both_strikes

# Repeated values collapse: a fly's body is one strike, not two.
run_mutation "pricer package: repeats are not collapsed" \
  crates/geode-pricer/src/core/package.rs \
  '        match keys.iter().position(|k| *k == v) {' \
  '        match None::<usize> {' \
  geode-pricer distinct_values_keep_leg_order_and_collapse_repeats

# Barrier columns read only barrier legs.
run_mutation "pricer package: a vanilla leg counts in a barrier column" \
  crates/geode-pricer/src/core/package.rs \
  '            Instrument::Vanilla(_) => None,' \
  '            Instrument::Vanilla(v) => Some((l, unreachable_barrier(v))),' \
  geode-pricer barrier_columns_read_only_barrier_legs
```

The third anchor's replacement must compile. If writing it would need a helper, choose a different mutation that makes a vanilla leg count: for example, set the `Barrier` arm to `None` instead, which empties the column. Any mutation that makes the test fail proves the rule. Commit before running `zsh scripts/mutation-check.sh "pricer package"`; every entry must be caught. `--anchors-only` must exit 0.

- [ ] **Step 6: Commit**

```bash
cargo fmt && cargo clippy -p geode-pricer --all-targets -- -D warnings
git add -A crates/geode-pricer scripts/mutation-check.sh
git commit -m "feat(pricer): package rows show their legs' values joined with /

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: Package edits (`core::package::commit`, `cell` routing, nudge)

**Files:**
- Modify: `crates/geode-pricer/src/core/package.rs` (`commit`, `editor_text`)
- Modify: `crates/geode-pricer/src/core/cell.rs`:
  - `edit_for` and `changed` become `pub(crate)`;
  - `editor_for` routes package rows;
  - new `commit_edits`;
  - `nudge` refuses lists.
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Consumes: `groups`, `package_qty`, `aggregates` (Task 1).
- Produces:
  - `pub fn editor_text(sheet: &Sheet, row: usize, kind: ColumnKind) -> Option<String>`, the editor's opening text (the groups' `edit` spellings joined with `/`, or the package qty). `None` when the column does not aggregate.
  - `pub fn commit(sheet: &Sheet, row: usize, kind: ColumnKind, text: &str) -> Result<Vec<Edit>, String>`
  - `cell::commit_edits(sheet, row, kind, text) -> Result<Vec<Edit>, String>`: a package row goes to `package::commit`; a line row gives `commit(...)` as zero or one edit.

- [ ] **Step 1: Failing tests** (append to `package.rs` tests)

```rust
    fn apply(s: &mut Sheet, row: usize, name: &str, text: &str) -> Result<usize, String> {
        let kind = column(name).unwrap().kind;
        let edits = commit(s, row, kind, text)?;
        let n = edits.len();
        for e in edits {
            s.apply(e).unwrap();
        }
        Ok(n)
    }

    #[test]
    fn a_single_value_goes_to_every_leg() {
        let mut s = sheet_of(&["-5 SPX Z26 7400/7800 CS"]);
        assert_eq!(apply(&mut s, 0, "underlying", "sx5e"), Ok(2));
        assert_eq!(text(&s, 0, "underlying").0, "SX5E");
        assert_eq!(apply(&mut s, 0, "expiry", "H27"), Ok(2));
        assert_eq!(text(&s, 0, "expiry").0, "H27");
        assert_eq!(apply(&mut s, 0, "strike", "7600"), Ok(2));
        assert_eq!(text(&s, 0, "strike").0, "7600", "both legs on one strike");
    }

    #[test]
    fn a_list_maps_by_position_and_a_fly_body_moves_once() {
        let mut s = sheet_of(&["-5 SPX Z26 7400/7800 CS"]);
        assert_eq!(apply(&mut s, 0, "strike", "7500/7900"), Ok(2));
        assert_eq!(text(&s, 0, "strike").0, "7500/7900");
        assert_eq!(s.shorthand(0), "-5 SPX Z26 7500/7900 CS", "still a CS");
        let mut f = sheet_of(&["SPX Z26 7400/7600/7800 FLY"]);
        assert_eq!(apply(&mut f, 0, "strike", "7300/7600/7900"), Ok(2), "the body is unchanged: two edits");
        assert_eq!(f.shorthand(0), "SPX Z26 7300/7600/7900 FLY");
        let mut c = sheet_of(&["SPX Z26/H27 7600 CAL"]);
        let before = text(&c, 0, "expiry").0;
        let parts: Vec<&str> = before.split('/').collect();
        assert_eq!(apply(&mut c, 0, "expiry", &format!("{}/{}", parts[0], "M27")), Ok(1));
    }

    #[test]
    fn a_wrong_count_or_a_bad_part_refuses_and_changes_nothing() {
        let mut s = sheet_of(&["-5 SPX Z26 7400/7800 CS"]);
        assert_eq!(
            apply(&mut s, 0, "strike", "7400/7600/7800"),
            Err("2 values: 7400/7800".into())
        );
        assert!(apply(&mut s, 0, "strike", "7500/abc").is_err());
        assert_eq!(text(&s, 0, "strike").0, "7400/7800", "nothing applied");
    }

    #[test]
    fn package_qty_rescales_legs_by_weight() {
        let mut f = sheet_of(&["SPX Z26 7400/7600/7800 FLY"]);
        assert_eq!(apply(&mut f, 0, "qty", "10"), Ok(3));
        assert_eq!(f.shorthand(0), "10 SPX Z26 7400/7600/7800 FLY");
        let legs: Vec<i64> = f.children(0).map(|l| f.qty(l)).collect();
        assert_eq!(legs, [10, -20, 10]);
        assert_eq!(apply(&mut f, 0, "qty", "1/2"), Err("one quantity".into()));
        assert_eq!(apply(&mut f, 0, "qty", "0"), Err("quantity must not be zero".into()));
        assert_eq!(apply(&mut f, 0, "qty", &i64::MAX.to_string()), Err("quantity out of range".into()));
    }

    #[test]
    fn qty_in_list_form_maps_by_position() {
        let mut s = sheet_of(&["-5 SPX Z26 7400/7800 CS"]);
        s.apply(Edit::SetQty { row: 2, qty: 3 }).unwrap();
        assert_eq!(apply(&mut s, 0, "qty", "-4/4"), Ok(2));
        assert_eq!(text(&s, 0, "qty").0, "-4", "back in CS form");
    }

    #[test]
    fn an_unchanged_commit_is_no_edit_and_crossed_strikes_are_allowed() {
        let mut s = sheet_of(&["-5 SPX Z26 7400/7800 CS"]);
        assert_eq!(apply(&mut s, 0, "strike", "7400/7800"), Ok(0));
        // Crossed strikes are allowed (the CS table has no strike order, so
        // it still prints as a CS, now long the higher strike).
        assert_eq!(apply(&mut s, 0, "strike", "7800/7400"), Ok(2));
        assert_eq!(s.shorthand(0), "-5 SPX Z26 7800/7400 CS");
        // A change that breaks the table keeps the name and prints the legs.
        assert_eq!(apply(&mut s, 0, "type", "P/C"), Err("1 values: C".into()), "one type shown: one value or refused");
        assert_eq!(apply(&mut s, 0, "type", "P"), Ok(2));
        assert!(s.shorthand(0).contains('\n'), "puts no longer fit CS: legs one per line");
        assert_eq!(s.kind(0), crate::core::sheet::RowKind::Package { template: Template::CS }, "keeps its name");
    }

    #[test]
    fn the_editor_opens_on_the_line_editors_spellings() {
        let s = sheet_of(&["-5 SPX Z26 7400/7800 CS"]);
        assert_eq!(editor_text(&s, 0, ColumnKind::Strike).as_deref(), Some("7400/7800"));
        assert_eq!(editor_text(&s, 0, ColumnKind::Qty).as_deref(), Some("-5"));
        assert_eq!(editor_text(&s, 0, ColumnKind::Price), None);
    }
```

In `cell.rs` tests, add:

```rust
    #[test]
    fn a_package_opens_a_text_editor_even_for_expiry_and_type_and_a_list_does_not_nudge() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![callspread(1)]);
        assert_eq!(editor_for(&s, 0, ColumnKind::Expiry), Ok(CellEditor::Text("Z26".into())));
        assert_eq!(editor_for(&s, 0, ColumnKind::Type), Ok(CellEditor::Text("C".into())));
        assert_eq!(editor_for(&s, 0, ColumnKind::Price), Err(READ_ONLY));
        assert_eq!(nudge(ColumnKind::Strike, "4800/5200", 1), Err("a list does not nudge".into()));
        assert_eq!(nudge(ColumnKind::Strike, "4800", 1).as_deref(), Ok("4801"));
    }
```

The existing tests that expect package cells to be `READ_ONLY` change. `results_packages_and_barrier_columns_on_a_vanilla_are_read_only` asserts `editor_for(&s, 1, kind) == Err(READ_ONLY)` for every package kind, and `a_date_commit_on_a_package_is_read_only` covers dates. Narrow them to the result columns and `commit_date`: a package's expiry edits through text, so `commit_date` on a package stays `READ_ONLY`. Keep their other assertions.

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p geode-pricer`. It fails to compile.

- [ ] **Step 3: Implement**

`package.rs`:

```rust
/// The text the editor opens on: the package quantity while the legs fit
/// the template, else the groups in the line editor's spellings.
pub fn editor_text(sheet: &Sheet, row: usize, kind: ColumnKind) -> Option<String> {
    if !aggregates(kind) {
        return None;
    }
    if kind == ColumnKind::Qty
        && let Some((q, _)) = package_qty(sheet, row)
    {
        return Some(q.to_string());
    }
    let format = crate::core::columns::column_for_kind(kind).default_format.clone();
    Some(
        groups(sheet, row, kind, &format)
            .iter()
            .map(|g| g.edit.as_str())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

/// The edits a committed package cell means (see the module doc): every
/// part validated through the line cell's own `edit_for` before anything
/// is returned; an empty vector is no change.
pub fn commit(sheet: &Sheet, row: usize, kind: ColumnKind, text: &str) -> Result<Vec<Edit>, String> {
    if !aggregates(kind) {
        return Err(crate::core::cell::READ_ONLY.into());
    }
    let t = text.trim();
    if kind == ColumnKind::Qty
        && let Some((_, weights)) = package_qty(sheet, row)
    {
        if t.contains('/') {
            return Err("one quantity".into());
        }
        let q: i64 = t.parse().map_err(|_| format!("quantity '{t}' is not a whole number"))?;
        if q == 0 {
            return Err("quantity must not be zero".into());
        }
        let mut edits = Vec::new();
        for (leg, w) in sheet.children(row).zip(weights) {
            let qty = q.checked_mul(w).ok_or("quantity out of range")?;
            if qty != sheet.qty(leg) {
                edits.push(Edit::SetQty { row: leg, qty });
            }
        }
        return Ok(edits);
    }
    let format = crate::core::columns::column_for_kind(kind).default_format.clone();
    let gs = groups(sheet, row, kind, &format);
    if gs.is_empty() {
        return Err(crate::core::cell::READ_ONLY.into());
    }
    let parts: Vec<&str> = t.split('/').collect();
    let part = |i: usize| -> &str {
        if parts.len() == 1 { parts[0] } else { parts[i] }
    };
    if parts.len() != 1 && parts.len() != gs.len() {
        let current = gs.iter().map(|g| g.display.as_str()).collect::<Vec<_>>().join("/");
        return Err(format!("{} values: {current}", gs.len()));
    }
    let mut edits = Vec::new();
    for (i, g) in gs.iter().enumerate() {
        for &leg in &g.legs {
            let edit = crate::core::cell::edit_for(sheet, leg, kind, part(i))?;
            if let Some(edit) = crate::core::cell::changed(sheet, leg, edit) {
                edits.push(edit);
            }
        }
    }
    Ok(edits)
}
```

- `column_for_kind` stands for however the crate finds a column's `ColumnDef` from its `ColumnKind`. Look it up in `columns.rs`; if no helper exists, iterate `COLUMNS` for the matching kind.
- The format only affects the display spellings used in the count message, so any column format for that kind is fine.
- The `?` on `edit_for`'s `String` error propagates the line cell's message for the bad part.

`cell.rs`:
- `edit_for` and `changed` become `pub(crate)`.
- At the top of `editor_for`:

```rust
    if sheet.is_package(row) {
        return crate::core::package::editor_text(sheet, row, kind)
            .map(CellEditor::Text)
            .ok_or(READ_ONLY);
    }
```

- A new function:

```rust
/// A committed cell's edits: a package row's through `package::commit`
/// (one per leg that changes), a line's as zero or one. The tile applies
/// several as one undo entry.
pub fn commit_edits(sheet: &Sheet, row: usize, kind: ColumnKind, text: &str) -> Result<Vec<Edit>, String> {
    if sheet.is_package(row) {
        return crate::core::package::commit(sheet, row, kind, text);
    }
    commit(sheet, row, kind, text).map(|e| e.into_iter().collect())
}
```

- At the top of `nudge`, after trimming: `if t.contains('/') { return Err("a list does not nudge".into()); }`

- [ ] **Step 4: Run**

Run: `cargo test -p geode-pricer`. All pass.

- [ ] **Step 5: Mutation entries**

```zsh
# A list maps by position onto the groups.
run_mutation "pricer package: a list edit uses the first part for every group" \
  crates/geode-pricer/src/core/package.rs \
  '        if parts.len() == 1 { parts[0] } else { parts[i] }' \
  '        parts[0]' \
  geode-pricer a_list_maps_by_position_and_a_fly_body_moves_once

# A list whose count matches neither one nor the groups is refused.
run_mutation "pricer package: a wrong count is accepted" \
  crates/geode-pricer/src/core/package.rs \
  '    if parts.len() != 1 && parts.len() != gs.len() {' \
  '    if false {' \
  geode-pricer a_wrong_count_or_a_bad_part_refuses_and_changes_nothing

# Package qty rescales the legs by weight.
run_mutation "pricer package: package qty sets every leg to q" \
  crates/geode-pricer/src/core/package.rs \
  '            let qty = q.checked_mul(w).ok_or("quantity out of range")?;' \
  '            let qty = q.checked_mul(w.signum()).ok_or("quantity out of range")?;' \
  geode-pricer package_qty_rescales_legs_by_weight
```

Use the anchors exactly as rustfmt leaves the code: the `part` closure may be reformatted onto several lines. Commit before running `zsh scripts/mutation-check.sh "pricer package"`; every entry must be caught. `--anchors-only` must exit 0.

- [ ] **Step 6: Commit**

```bash
cargo fmt && cargo clippy -p geode-pricer --all-targets -- -D warnings
git add -A crates/geode-pricer scripts/mutation-check.sh
git commit -m "feat(pricer): package cells edit their legs by position

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: The tile commits a package edit as one undo step

**Files:**
- Modify: `crates/geode-pricer/src/tile.rs`:
  - `commit_edit` calls `cell::commit_edits`;
  - `finish_commit` takes `Result<Vec<Edit>, String>`;
  - `commit_date` wraps its `Option`.
- Modify: `docs/current/features.md`, `crates/geode-pricer/README.md`
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Consumes: `cell::commit_edits`, `cell::commit_date` (Task 2).

- [ ] **Step 1: Failing tile tests**

Use `open_seeded(cx, &["-5 SPX Z26 7400/7800 CS", "SPX Z26 4000 P"])` and the existing helpers `typed`, `h.dispatch`, `h.footer`, `h.sheet_len`, and `h.prices()` for submitted price batches.

Put the cursor on the package row (row 0) in the strike column, using the same way other tile tests move to a column: `right` steps or `h.dispatch(&mut vcx, "last_col", …)`. Find the strike column's index from `h.columns(&vcx)`.

```rust
    #[gpui::test]
    fn editing_a_package_strike_moves_both_legs_in_one_undo_step(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &["-5 SPX Z26 7400/7800 CS", "SPX Z26 4000 P"]);
        let _ = h.prices();
        goto_column(&h, &mut vcx, "strike");
        h.dispatch(&mut vcx, "edit", None);
        assert_eq!(editor_text(&h, &vcx).as_deref(), Some("7400/7800"));
        select_all_and_type(&h, &mut vcx, "7500/7900");
        h.dispatch(&mut vcx, "commit", None);
        let spread = h.tile.read_with(&vcx, |t, _| t.sheet.shorthand(0));
        assert_eq!(spread, "-5 SPX Z26 7500/7900 CS");
        assert_eq!(h.prices().len(), 1, "one reprice for the whole commit");
        h.dispatch(&mut vcx, "undo", None);
        let spread = h.tile.read_with(&vcx, |t, _| t.sheet.shorthand(0));
        assert_eq!(spread, "-5 SPX Z26 7400/7800 CS", "one undo restores both legs");
    }

    #[gpui::test]
    fn a_package_expiry_opens_a_text_editor_and_a_bad_count_says_so(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &["-5 SPX Z26 7400/7800 CS"]);
        goto_column(&h, &mut vcx, "expiry");
        h.dispatch(&mut vcx, "edit", None);
        assert!(h.tile.read_with(&vcx, |t, _| t.date_field().is_none()), "not the date field");
        goto_column_after_cancel(&h, &mut vcx, "strike");
        h.dispatch(&mut vcx, "edit", None);
        select_all_and_type(&h, &mut vcx, "1/2/3");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.footer(&vcx).as_deref(), Some("2 values: 7400/7800"));
        assert_eq!(h.mode(&mut vcx), "insert", "the editor stays open");
    }
```

- `goto_column`, `goto_column_after_cancel`, `editor_text` and `select_all_and_type` stand for this file's existing ways of doing each. Reuse them if they exist, and write small local helpers if not:
  - moving to a column: `h.columns()` gives the index; dispatch `first_col` then `right` n times;
  - cancelling: dispatch `cancel`;
  - reading the open editor's text;
  - replacing the field's text: select-all keystrokes (`cmd-a` or `ctrl-a` per the crate's other tests) then `typed`.
- `h.prices()` returns the price batches submitted since the last drain. Confirm that one commit gives one batch; if the harness counts differently, assert whatever proves a single reprice.

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p geode-pricer editing_a_package_strike a_package_expiry_opens`. The first fails, because `apply_edit` is handed only the first edit or the commit is refused.

- [ ] **Step 3: Implement**

In `commit_edit`, replace `let answer = cell::commit(&self.sheet, row, kind, &value);` with `let answer = cell::commit_edits(&self.sheet, row, kind, &value);`.

`commit_date` passes `answer.map(|e| e.into_iter().collect())`.

`finish_commit`:

```rust
    /// Settle an editor's parsed commit: a refusal keeps the editor open
    /// with the reason; no edits — every value unchanged — closes it with
    /// no undo entry, reprice or save; edits close it (blur first, so the
    /// rebuild never paints a dead field) and apply as ONE undo entry
    /// (a package commit edits each leg that changes).
    fn finish_commit(
        &mut self,
        answer: Result<Vec<Edit>, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match answer {
            Err(why) => {
                self.footer = Some(why.into());
                self.rebuild_chrome();
                cx.notify();
            }
            Ok(edits) if edits.is_empty() => {
                self.close_editor(window, cx);
                self.rebuild_chrome();
                cx.notify();
            }
            Ok(mut edits) => {
                self.close_editor(window, cx);
                let result = if edits.len() == 1 {
                    self.apply_edit(edits.pop().expect("one"), cx)
                } else {
                    self.apply_edits(edits, cx)
                };
                if let Err(e) = result {
                    self.footer = Some(e.to_string().into());
                    self.rebuild_chrome();
                    cx.notify();
                }
            }
        }
    }
```

Check `apply_edits`' doc and body: it must record ONE undo entry and reprice once. If it doesn't, stop and report; don't change its contract.

- [ ] **Step 4: Run**

Run: `cargo test -p geode-pricer`, then `cargo test -p geode-app pricer`. All pass.

- [ ] **Step 5: Mutation entry**

```zsh
# A package commit's edits apply as one undo entry.
run_mutation "pricer tile: a package commit applies only its first edit" \
  crates/geode-pricer/src/tile.rs \
  '                    self.apply_edits(edits, cx)' \
  '                    self.apply_edit(edits.remove(0), cx)' \
  geode-pricer editing_a_package_strike_moves_both_legs_in_one_undo_step
```

Commit before running it; it must be caught. `--anchors-only` must exit 0.

- [ ] **Step 6: Docs**

`docs/current/features.md`, pricer section: change the sentence "Package rows are read-only in every column" to:

"A package row shows its legs' distinct values in each text column, in
leg order and joined with `/` (a call spread reads `SPX`, `Z26`,
`7400/7800`, `C`), and its qty column shows the package quantity while
the legs fit the package's template (else the legs' quantities joined with
`/`). These cells edit (`i`, `enter`, double-click open a text editor):
one value goes to every leg; a `/` list with one part per shown value
replaces each where it appears (`7500/7900` moves a spread's two strikes;
a fly's body moves once); a package quantity rescales every leg by its
weight. Every part is checked first, and the whole edit is one undo step
and one reprice. Result columns stay read-only."

`crates/geode-pricer/README.md`:
- Add a module-map row: `package`: "A package row's aggregated cells and how an edit maps onto its legs."
- Add an invariant: "A package cell's edit maps by position onto the distinct values it shows, validates every part through the line cell's `edit_for`, and applies as one undo entry."

- [ ] **Step 7: Commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings
git add -A crates/geode-pricer scripts/mutation-check.sh docs/current/features.md
git commit -m "feat(pricer): a package cell edit applies to its legs as one undo step

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: Branch verification (controller)

- [ ] Run `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo check -p geode-shell --features test-support --all-targets`, `zsh scripts/mutation-check.sh --anchors-only`, and `zsh scripts/mutation-check.sh --changed=<branch base>` (detached).
- [ ] Display checks for the user:
  - package rows reading `SPX · Z26 · 7400/7800 · C` in their muted package paint;
  - a long `/` list ending in `…` in a narrow column;
  - an edit on a package row: open, change and see the legs move;
  - the footer message on a wrong count.
