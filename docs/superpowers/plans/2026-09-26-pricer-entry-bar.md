# Pricer Entry Bar Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the pricer's in-table `o` placeholder row with an entry
bar under the header, and cut column 0 down to structure (indent, chevron,
package template token).

**Architecture:** `core::entry` keeps the pure half: where a line lands
and what the bar's label says. The tile keeps its `Entry` state but paints
it in a strip between the header and the `DataTable` instead of as a grid
row, so `GridRowKind::Entry` and every "the row slid up when the entry
closed" path go away. `GridRow::tree` splits into a painted `tag` and an
unpainted `search` key that find reads.

**Tech Stack:** Rust, GPUI 0.2 + gpui-component 0.6.2 (`Input`,
`InputState`, `InputEvent`, `DataTable`), the repo's zsh mutation harness.

**Spec:** `docs/superpowers/specs/2026-09-26-pricer-entry-bar-design.md`

## Global Constraints

- Keep the action id `pricer::add_below` and its `o` binding: `geode-app`
  tests (`bridge.rs` `typing_into_the_pricer_entry_field_fires_no_shell_binding`,
  `main.rs` registry test) dispatch it. Retitle it `Add lines…`.
- Remove `pricer::add_above` and its `shift+o` binding entirely.
- A field blurs before it drops (CLAUDE.md). `close_entry` keeps its
  blur-then-drop order.
- No I/O, allocation or formatting in render. The bar's label is a
  `SharedString` computed when the place changes.
- Theme tokens only: `theme.border`, `theme.muted_foreground`,
  `chip_paint(theme, Tone::DangerText).text`. No literal colors.
- Every behavior change updates `docs/current/features.md` and
  `crates/geode-pricer/README.md` in the same task.
- Edit `scripts/mutation-check.sh` entries whose anchors or tests change;
  run `zsh scripts/mutation-check.sh --anchors-only` at the end of each
  task that touches anchored code. It must exit 0.
- Commit messages end with
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Work in a worktree off `main` (`EnterWorktree`), never on `main`.

## Review Focus

1. A palette `Add lines…` while the bar is open: `dispatch` closes fields
   for any non-field verb first, so the bar closes and reopens at the
   cursor. Expect: one bar, focused, empty, label recomputed.
2. A refused insert (a package typed at a leg place) must restore
   `entry.place` and show the error in the bar, not the footer. The
   footer must stay as it was.
3. `up`/`down` history uses `set_value`, which emits no `Change`, so a
   standing error would survive a history step. Expect: a history step
   clears the error explicitly.
4. `:` commands and palette verbs close the bar (existing `close_entry`
   calls in `command` and `dispatch`). Expect focus to leave the field,
   mode `normal`.
5. With line numbers on, the gutter numbers every painted row with no
   entry gap (the `number_rows` signature loses its `entry` argument).

Tests for 1–3 are in Task 2; 4 is covered by the existing
`a_click_on_the_table_cancels_the_entry_and_another_verb_closes_it`;
5 by the updated line-numbers test in Task 2.

---

### Task 1: Pure placement and label in `core::entry`

**Files:**
- Modify: `crates/geode-pricer/src/core/entry.rs`
- Modify: `scripts/mutation-check.sh` (one new entry)

**Interfaces:**
- Produces: `pub fn target_label(sheet: &Sheet, place: Place) -> String`
  in `crate::core::entry`. `place_for` keeps its current signature in
  this task; its `None` arm changes.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `crates/geode-pricer/src/core/entry.rs`:

```rust
    #[test]
    fn with_no_cursor_row_a_line_lands_at_the_end() {
        let s = sheet();
        assert_eq!(place_for(&s, None, true), Place::Root { at: 5 });
        let empty = Sheet::new("t");
        assert_eq!(place_for(&empty, None, true), Place::Root { at: 0 });
    }

    /// [A, P(L1, L2), B] — flat rows 0..5.
    #[test]
    fn the_label_names_where_enter_lands() {
        let s = sheet();
        assert_eq!(target_label(&s, Place::Root { at: 5 }), "at end");
        assert_eq!(
            target_label(&s, Place::Root { at: 1 }),
            format!("after {}", s.shorthand(0))
        );
        assert_eq!(
            target_label(&s, Place::Root { at: 4 }),
            format!("after {}", s.shorthand(1)),
            "a root after a package names the package, not its last leg"
        );
        assert_eq!(
            target_label(&s, Place::Leg { package: 1, leg: 0 }),
            "into CS"
        );
        assert_eq!(
            target_label(&s, Place::Leg { package: 1, leg: 2 }),
            format!("after {}", s.shorthand(3))
        );
        assert_eq!(
            target_label(&Sheet::new("t"), Place::Root { at: 0 }),
            "at end"
        );
    }

    #[test]
    fn a_custom_package_is_named_by_its_token_in_the_label() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1)]);
        push(&mut s, vec![line(spx(4000.0, OptionKind::Put), 1)]);
        s.apply(crate::core::Edit::Group {
            first: 0,
            count: 2,
            template: crate::core::Template::Custom,
            id: None,
        })
        .unwrap();
        assert_eq!(target_label(&s, Place::Root { at: 3 }), "at end");
        s.apply(crate::core::Edit::Insert {
            place: Place::Root { at: 3 },
            rows: vec![parse("SPX Z26 3000 P").unwrap()],
        })
        .unwrap();
        assert_eq!(target_label(&s, Place::Root { at: 3 }), "after CUSTOM");
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-pricer core::entry`
Expected: compile error, `target_label` not found.

- [ ] **Step 3: Implement**

In `place_for`, change the `None` arm:

```rust
    let Some(row) = row else {
        return Place::Root { at: sheet.len() };
    };
```

and its doc line to: `` `None` (no cursor row) lands at the end of the sheet. ``

Update the existing assertion in
`o_lands_after_the_cursor_row_and_shift_o_before_it` from
`Place::Root { at: 0 }, "an empty sheet"` to
`Place::Root { at: 5 }, "no cursor row: the end"`.

Add after `next_place`:

```rust
/// A row as the bar's label names it: its shorthand, or its template
/// token when the shorthand is empty or spans several lines (a custom
/// package's legs, one per line).
fn describe(sheet: &Sheet, row: usize) -> String {
    let text = sheet.shorthand(row);
    if !text.is_empty() && !text.contains('\n') {
        return text;
    }
    match sheet.kind(row) {
        RowKind::Package { template } => template.token().to_string(),
        _ => text,
    }
}

/// Where the entry bar's `enter` lands, as its muted label (entry-bar
/// spec §4.2). Computed from the place alone, so the label and the insert
/// can never disagree.
pub fn target_label(sheet: &Sheet, place: Place) -> String {
    match place {
        Place::Root { at } if at == 0 || at >= sheet.len() => "at end".to_string(),
        Place::Root { at } => {
            let before = at - 1;
            let root = sheet.parent(before).unwrap_or(before);
            format!("after {}", describe(sheet, root))
        }
        Place::Leg { package, leg: 0 } => match sheet.kind(package) {
            RowKind::Package { template } => format!("into {}", template.token()),
            _ => format!("into {}", describe(sheet, package)),
        },
        Place::Leg { package, leg } => format!("after {}", describe(sheet, package + leg)),
    }
}
```

Extend the `use` line to
`use crate::core::sheet::{Place, RowKind, RowSpec, Sheet};`. `at == 0` with
a non-empty sheet cannot arise once `shift+o` goes (Task 2); it reads
`at end` rather than naming a row that is not before it.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p geode-pricer core::entry`
Expected: all pass.

- [ ] **Step 5: Mutation entry**

Append after the `pricer entry: o below a leg lands before it` entry in
`scripts/mutation-check.sh`:

```zsh
# With no cursor row a typed line lands at the end, where the bar's
# label says `at end`. Mutated, it lands at the top.
run_mutation "pricer entry: no cursor row lands at the start" \
  crates/geode-pricer/src/core/entry.rs \
  '        return Place::Root { at: sheet.len() };' \
  '        return Place::Root { at: 0 };' \
  geode-pricer with_no_cursor_row_a_line_lands_at_the_end

# A root after a package is named by the package, not its last leg.
run_mutation "pricer entry: the label names a leg for a root place" \
  crates/geode-pricer/src/core/entry.rs \
  '            let root = sheet.parent(before).unwrap_or(before);' \
  '            let root = before;' \
  geode-pricer the_label_names_where_enter_lands
```

Run: `zsh scripts/mutation-check.sh "pricer entry"`
Expected: every `pricer entry` entry reports caught.
Run: `zsh scripts/mutation-check.sh --anchors-only` — exit 0.

- [ ] **Step 6: Commit**

```bash
cargo fmt
git add crates/geode-pricer/src/core/entry.rs scripts/mutation-check.sh
git commit -m "feat(pricer): entry lands at the end with no cursor row; target label

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: The entry bar replaces the placeholder row

**Files:**
- Modify: `crates/geode-pricer/src/core/entry.rs` (`place_for` loses `below`)
- Modify: `crates/geode-pricer/src/content.rs` (actions, keymap)
- Modify: `crates/geode-pricer/src/tile.rs` (Entry, open/commit/close, render, clicks, tests)
- Modify: `crates/geode-pricer/src/header.rs` (`render_entry_bar`)
- Modify: `crates/geode-pricer/src/grid.rs` (drop `Entry` kind and placement)
- Modify: `crates/geode-pricer/src/delegate.rs` (drop entry mirror, `number_rows` arg)
- Modify: `scripts/mutation-check.sh`
- Modify: `docs/current/features.md`, `crates/geode-pricer/README.md`

**Interfaces:**
- Consumes: `target_label(&Sheet, Place) -> String` (Task 1).
- Produces:
  - `pub fn place_for(sheet: &Sheet, row: Option<usize>) -> Place`
  - `GridModel::build(sheet, expansion, plan, clock)` (no `entry`)
  - `number_rows(mode, len, cursor) -> Vec<Option<usize>>`
  - `header::render_entry_bar(input: &Entity<InputState>, label: &SharedString, error: Option<&SharedString>, theme: &Theme) -> impl IntoElement`
  - `Entry { place, input, label: SharedString, error: Option<SharedString>, .. }`
  - test harness: `Harness::entry_label(&self, &VisualTestContext) -> Option<String>`,
    `Harness::entry_error(..) -> Option<String>`, `Harness::entry_text(..) -> Option<String>`

- [ ] **Step 1: `place_for` loses `below`**

In `core/entry.rs`:

```rust
/// Where a new row lands: below the cursor row. On a leg, the next leg of
/// its package; on a package row, its first leg; `None` (no cursor row)
/// at the end of the sheet.
pub fn place_for(sheet: &Sheet, row: Option<usize>) -> Place {
    let Some(row) = row else {
        return Place::Root { at: sheet.len() };
    };
    if let Some(p) = sheet.parent(row) {
        return Place::Leg {
            package: p,
            leg: row - p,
        };
    }
    if sheet.is_package(row) {
        return Place::Leg {
            package: row,
            leg: 0,
        };
    }
    Place::Root { at: row + 1 }
}
```

Update the module doc's first line to "where `o` lands". Rename the test
`o_lands_after_the_cursor_row_and_shift_o_before_it` to
`o_lands_after_the_cursor_row` and reduce it to:

```rust
    #[test]
    fn o_lands_after_the_cursor_row() {
        let s = sheet();
        assert_eq!(place_for(&s, Some(0)), Place::Root { at: 1 });
        assert_eq!(
            place_for(&s, Some(1)),
            Place::Leg { package: 1, leg: 0 },
            "a package row: its first leg"
        );
        assert_eq!(place_for(&s, Some(2)), Place::Leg { package: 1, leg: 1 });
        assert_eq!(place_for(&s, Some(3)), Place::Leg { package: 1, leg: 2 });
        assert_eq!(place_for(&s, Some(4)), Place::Root { at: 5 });
    }
```

and drop the `, true` argument in `with_no_cursor_row_a_line_lands_at_the_end`.

- [ ] **Step 2: Remove `shift+o`**

In `content.rs`: delete `("pricer::add_above", "Add line above…"),`,
change `("pricer::add_below", "Add line below…")` to
`("pricer::add_below", "Add lines…")`, delete the keymap line
`"shift+o" = "pricer::add_above"`. Add to `content.rs`'s test module:

```rust
    #[test]
    fn shift_o_is_unbound_and_add_above_is_gone() {
        assert!(!DEFAULT_KEYMAP.contains("shift+o"));
        assert!(!ACTIONS.iter().any(|(id, _)| *id == "pricer::add_above"));
    }
```

(If `content.rs` has no `#[cfg(test)] mod tests`, add one with
`use super::*;`.)

- [ ] **Step 3: Grid model drops the placeholder**

In `grid.rs`:
- Delete the `Entry` variant of `GridRowKind` and its doc.
- `GridModel::build(sheet, expansion, plan, clock)`: delete the `entry`
  parameter, the `placeholder` closure, `pending`, both `if let Some(..)
  = pending` blocks, `fn flat`, `entry_row`, the review-fix doc paragraph
  on `build`, and the module doc's placeholder paragraph.
  `Vec::with_capacity(visible.len())`.
- `use crate::core::sheet::{LineId, RowKind, Sheet};` (no `Place`).
- Tests: `fn build(s: &Sheet, e: &Expansion) -> GridModel { GridModel::build(s, e, &plan(), Clock::utc()) }`;
  drop the `None` argument at every call; delete
  `the_entry_placeholder_paints_where_its_place_lands_and_is_no_sheet_row`
  and `a_leg_entry_into_a_closed_package_lands_after_it_as_a_fallback`.

- [ ] **Step 4: Delegate drops the entry mirror**

In `delegate.rs`:
- Delete the `entry: Option<Entity<InputState>>` field, its doc, and
  `entry: None` in `new`.
- Replace `number_rows` with:

```rust
/// The gutter number of every painted grid row. Relative mode measures
/// from the cursor row; with no cursor row it numbers absolutely. Off
/// numbers nothing.
pub(crate) fn number_rows(
    mode: LineNumbers,
    len: usize,
    cursor: Option<usize>,
) -> Vec<Option<usize>> {
    let (mode, at) = match (mode, cursor) {
        (LineNumbers::Relative, Some(c)) => (mode, c),
        (LineNumbers::Relative, None) => (LineNumbers::On, 0),
        (mode, _) => (mode, 0),
    };
    (0..len).map(|row| gutter_number(mode, row, at)).collect()
}
```

- `type NumbersStamp = (usize, Option<usize>, LineNumbers);` with doc
  "row count, the cursor row (relative mode only), and the mode".
- In `refresh_numbers`: delete `let entry = …`;
  `let stamp = (len, cursor, mode);`;
  `self.gutter = gutter_px(mode, len);`;
  `number_rows(mode, len, cursor)`.
- `render_tr`: delete the `Some(GridRowKind::Entry) => …` arm; doc
  becomes "A package row's ground, on the row".
- `render_cell` tree branch: delete the `if row.kind == GridRowKind::Entry { … }`
  block and the "except the entry placeholder…" clause of its comment.
- Module doc: drop "entry field," and "and entry" mentions.
- `cell_input` doc: "An in-grid text field" stays (the cell editor uses it).
- Tests: drop the `None` second argument from every `number_rows` call
  (`number_rows(LineNumbers::On, 5, Some(3))` etc.) and delete
  `the_entry_placeholder_is_blank_and_shifts_nothing`.
- `use gpui_component::input::{Input, InputState};` stays (the editor
  uses both).

- [ ] **Step 5: The bar's painter**

Add to `header.rs` after `render_footer` (import
`gpui_component::input::{Input, InputState}`, `gpui_component::v_flex`,
`geode_shell::fonts`, `gpui::relative`):

```rust
/// The entry bar (entry-bar spec §4) between the header and the table:
/// where `enter` lands, muted, then a one-line borderless field, and a
/// refused `enter`'s reason under it in danger text. The reason sits
/// beside the text that caused it rather than in the footer, as the
/// timeseries expression strip does.
pub(crate) fn render_entry_bar(
    input: &Entity<InputState>,
    label: &SharedString,
    error: Option<&SharedString>,
    theme: &Theme,
) -> impl IntoElement {
    let danger = chip_paint(theme, Tone::DangerText).text;
    v_flex()
        .w_full()
        .px_2()
        .py_1()
        .gap_0p5()
        .border_b_1()
        .border_color(theme.border)
        .debug_selector(|| "pricer-entry".into())
        .child(
            h_flex()
                .w_full()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .flex_shrink_0()
                        .max_w(relative(0.4))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .debug_selector(|| "pricer-entry-label".into())
                        .child(label.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .font_family(fonts::MONO)
                        .debug_selector(|| "pricer-entry-field".into())
                        .child(Input::new(input).appearance(false).w_full()),
                ),
        )
        .when_some(error.cloned(), |el, e| {
            el.child(
                div()
                    .text_xs()
                    .text_color(danger)
                    .debug_selector(|| "pricer-entry-error".into())
                    .child(e),
            )
        })
}
```

- [ ] **Step 6: Tile state and routes**

In `tile.rs`:

1. `use crate::core::entry::{history, next_place, place_for, target_label};`
   and add `InputEvent` to the `gpui_component::input` import.
2. `Entry` gains, after `input`:

```rust
    /// Where `enter` lands, as the bar's muted label: `target_label` of
    /// `place`, rebuilt whenever `place` changes, never in render.
    pub label: SharedString,
    /// A refused `enter`'s reason, under the field. Any typed edit clears
    /// it (it describes text that is no longer there), as does a history
    /// step.
    pub error: Option<SharedString>,
```

   Doc on `Entry`: "The entry bar (entry-bar spec §4): where its rows
   will land, the field, and the sheet's own lines to walk with
   `up`/`down`."
3. Replace `open_entry`:

```rust
    /// `o`: the entry bar under the header, the field focused (entry-bar
    /// spec §4.1). Lines land below the cursor row; a leg place opens
    /// its package so what lands is visible.
    fn open_entry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading {
            self.footer = Some("the sheet is still loading".into());
            return;
        }
        self.close_entry(window, cx);
        let place = place_for(&self.sheet, self.cursor_sheet_row());
        if let Place::Leg { package, .. } = place {
            self.expansion.set(self.sheet.id(package), true);
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(ENTRY_HINT));
        cx.subscribe_in(&input, window, |this, _input, event, _window, cx| {
            if let InputEvent::Change = event
                && let Some(entry) = this.entry.as_mut()
                && entry.error.take().is_some()
            {
                cx.notify();
            }
        })
        .detach();
        input.read(cx).focus_handle(cx).focus(window, cx);
        self.entry = Some(Entry {
            place,
            input,
            label: target_label(&self.sheet, place).into(),
            error: None,
            history: history(&self.sheet),
            history_ix: None,
        });
        self.rebuild(cx);
    }
```

4. In `commit_entry`: the parse-error arm becomes

```rust
            Err(e) => {
                entry.error = Some(format!("{} (column {})", e.message, e.offset + 1).into());
                cx.notify();
                return;
            }
```

   In the `Ok(())` arm's `if let Some(entry) = self.entry.as_mut()` block
   add `entry.label = target_label(&self.sheet, entry.place).into();` and
   `entry.error = None;`. The `Err(e)` arm becomes

```rust
            Err(e) => {
                if let Some(entry) = self.entry.as_mut() {
                    entry.place = at;
                    entry.error = Some(e.to_string().into());
                }
                self.rebuild(cx);
            }
```

   Its doc: "`enter`: parse, insert, reprice, and advance the place past
   what landed; a parse error or a refusal keeps the text and says why
   under the field."
5. `close_entry`: delete the `self.table.update(… delegate_mut().entry = None)` line.
6. `step_history`: after `entry.history_ix = next;` add `entry.error = None;`.
7. `dispatch`: `"add_below" => { self.open_entry(window, cx); return true; }`
   (delete `| "add_above"`).
8. `rebuild`: `GridModel::build(&self.sheet, &self.expansion, &self.plan, self.clock)`;
   delete `entry_place`.
9. `cursor_rows`: drop the `.filter(|(_, r)| r.kind != GridRowKind::Entry)`
   and simplify to `(0..self.model.rows.len())`; doc "Grid rows a cursor
   may sit on". `yank_col`: drop its `.filter(...)`. Remove
   `GridRowKind` from imports if now unused.
10. Clicks: delete the fields `click_anchor` and `pressed` (and their
    initialisers). `line_at` doc: "Resolve a painted grid row to its
    LineId before closing fields." `chevron_clicked` doc drops the
    placeholder sentence. In `SelectCell` delete the "Entry closure
    shifts…" comment, `self.pressed = …`, and the `if self.entry.is_some()
    { self.click_anchor = … }` block. In `DoubleClickedCell` replace the
    `pressed` match with `let line = self.line_at(*row);`, delete the
    "A handed-on line wins" comment and the "Before the tree-column
    check" comment; keep the order of the remaining statements.
11. `render`: build the bar and put it between header and body:

```rust
        let bar = self.entry.as_ref().map(|e| {
            header::render_entry_bar(&e.input, &e.label, e.error.as_ref(), theme)
        });
```

    and `.child(header).children(bar).child(body).child(footer)`.
12. The `entry` field doc: "The entry bar (entry-bar spec §4): `o` opens
    it, `enter` parses and inserts through `apply_edit`, `escape`, a
    click or another verb drops it. `None` in normal mode."

- [ ] **Step 7: Tile tests**

Add harness helpers inside `impl Harness`:

```rust
        pub fn entry_text(&self, vcx: &VisualTestContext) -> Option<String> {
            self.tile.read_with(vcx, |t, cx| {
                t.entry.as_ref().map(|e| e.input.read(cx).value().to_string())
            })
        }
        pub fn entry_label(&self, vcx: &VisualTestContext) -> Option<String> {
            self.tile
                .read_with(vcx, |t, _| t.entry.as_ref().map(|e| e.label.to_string()))
        }
        pub fn entry_error(&self, vcx: &VisualTestContext) -> Option<String> {
            self.tile.read_with(vcx, |t, _| {
                t.entry.as_ref().and_then(|e| e.error.as_ref().map(|s| s.to_string()))
            })
        }
```

Rewrite the `// ---- the entry field ----` section's tests as follows
(keep `typed` and `focused`):

```rust
    /// Entry-bar spec §4: `o`, a line, `enter` adds a row below the
    /// cursor and keeps the bar open; the next `enter` lands below that.
    #[gpui::test]
    fn o_then_lines_then_enter_adds_each_below_the_last_and_keeps_the_bar_open(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.visible(&mut vcx, true);
        h.dispatch(&mut vcx, "add_below", None);
        assert_eq!(h.mode(&mut vcx), "insert");
        assert!(focused(&mut vcx), "the field owns focus");
        assert_eq!(h.entry_label(&vcx).as_deref(), Some("at end"));
        typed(&h, &mut vcx, "-5 SPX Z26 5000 C");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.sheet_len(&vcx), 1);
        let batches = h.prices();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].lines.len(), 1);
        assert_eq!(h.mode(&mut vcx), "insert", "the bar stays open");
        assert_eq!(h.entry_text(&vcx).as_deref(), Some(""));
        assert_eq!(h.entry_label(&vcx).as_deref(), Some("at end"));
        typed(&h, &mut vcx, "SPX Z26 4800/5200 CS");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.sheet_len(&vcx), 4, "a package with its two legs");
        assert_eq!(h.tree(&vcx).len(), 4, "the typed package opens; no placeholder row");
        assert_eq!(h.cursor(&vcx).map(|c| c.0), Some(1), "the cursor is on what landed");
    }

    #[gpui::test]
    fn o_lands_below_the_cursor_row_and_the_label_says_so(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "add_below", None);
        assert_eq!(h.entry_label(&vcx).as_deref(), Some("after SPX Z26 5000 C"));
        typed(&h, &mut vcx, "SPX Z26 3000 P");
        h.dispatch(&mut vcx, "commit", None);
        let second = h.tile.read_with(&vcx, |t, _| t.sheet.shorthand(1));
        assert_eq!(second, "SPX Z26 3000 P", "below row 0, above the package");
        assert_eq!(h.entry_label(&vcx).as_deref(), Some("after SPX Z26 3000 P"));
    }

    #[gpui::test]
    fn a_parse_error_shows_under_the_field_keeps_the_text_and_typing_clears_it(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.dispatch(&mut vcx, "add_below", None);
        typed(&h, &mut vcx, "SPX Z26 5000 CX");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.sheet_len(&vcx), 0);
        assert_eq!(h.mode(&mut vcx), "insert");
        let error = h.entry_error(&vcx).expect("the reason is under the field");
        assert!(error.ends_with("(column 14)"), "{error}");
        assert_eq!(h.footer(&vcx), None, "not in the footer");
        assert!(vcx.debug_bounds("pricer-entry-error").is_some());
        assert_eq!(h.entry_text(&vcx).as_deref(), Some("SPX Z26 5000 CX"));
        typed(&h, &mut vcx, "\u{8}");
        assert_eq!(h.entry_error(&vcx), None, "an edit answers the error");
    }

    #[gpui::test]
    fn a_history_step_clears_the_error(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "add_below", None);
        typed(&h, &mut vcx, "nonsense");
        h.dispatch(&mut vcx, "commit", None);
        assert!(h.entry_error(&vcx).is_some());
        h.dispatch(&mut vcx, "insert_up", None);
        assert_eq!(h.entry_error(&vcx), None);
    }

    #[gpui::test]
    fn o_on_a_leg_inserts_the_next_leg(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "down", None);
        h.dispatch(&mut vcx, "expand", None);
        h.dispatch(&mut vcx, "down", None); // the first leg
        h.dispatch(&mut vcx, "add_below", None);
        typed(&h, &mut vcx, "SPX Z26 5000 C");
        h.dispatch(&mut vcx, "commit", None);
        let legs = h.tile.read_with(&vcx, |t, _| t.sheet.children(1).len());
        assert_eq!(legs, 3);
        let middle = h.tile.read_with(&vcx, |t, _| t.sheet.shorthand(3));
        assert_eq!(middle, "SPX Z26 5000 C", "between the two legs");
    }

    #[gpui::test]
    fn a_package_typed_at_a_leg_place_is_refused_under_the_field(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "down", None);
        h.dispatch(&mut vcx, "add_below", None); // a package row: its first leg
        assert_eq!(h.entry_label(&vcx).as_deref(), Some("into CS"));
        typed(&h, &mut vcx, "SPX Z26 4800/5200 CS");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(
            h.entry_error(&vcx).as_deref(),
            Some("a package cannot hold a package")
        );
        assert_eq!(h.entry_label(&vcx).as_deref(), Some("into CS"), "the place is restored");
        assert_eq!(h.mode(&mut vcx), "insert");
    }

    #[gpui::test]
    fn a_palette_add_while_the_bar_is_open_reopens_it_at_the_cursor(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "add_below", None);
        typed(&h, &mut vcx, "half typed");
        h.dispatch(&mut vcx, "add_below", None);
        assert_eq!(h.mode(&mut vcx), "insert");
        assert!(focused(&mut vcx));
        assert_eq!(h.entry_text(&vcx).as_deref(), Some(""));
    }
```

Keep `up_and_down_walk_the_sheets_own_lines_newest_first`, replacing
its local `text` closure with `h.entry_text(vcx).unwrap()`.

Rename `escape_removes_the_placeholder_and_the_field_blurs_before_it_drops`
to `escape_closes_the_bar_and_the_field_blurs_before_it_drops`; replace
its `entry_row()` assertion with
`assert!(!painted(&mut vcx, "pricer-entry"), "the bar is gone");` and its
last message with `"the bar never held a row"`.

Replace `the_entry_field_paints_no_chrome` with:

```rust
    /// The bar paints between the header and the column headers.
    #[gpui::test]
    fn the_entry_bar_paints_between_the_header_and_the_table(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "add_below", None);
        h.draw(&mut vcx);
        let bar = vcx.debug_bounds("pricer-entry").expect("the bar is painted");
        let th = vcx.debug_bounds("pricer-th-1").expect("the headers are painted");
        assert!(bar.bottom() <= th.top(), "{bar:?} above {th:?}");
        let field = vcx.debug_bounds("pricer-entry-field").expect("the field");
        let label = vcx.debug_bounds("pricer-entry-label").expect("the label");
        assert!(label.right() <= field.left(), "the label leads the field");
    }
```

In `the_line_numbers_global_paints_a_gutter_beside_the_tree_column`
delete the block from `h.dispatch(&mut vcx, "add_above", None);` through
the `"the package keeps its number with the placeholder above it"`
assertion, and the doc sentence "The entry placeholder is blank and
shifts no number."

In `an_empty_table_names_the_next_action_or_that_it_is_loading`, after
`h.dispatch(&mut vcx, "add_below", None);` assert
`painted(&mut vcx, "pricer-empty")` with message
`"the bar is not a row; the empty text stays"`, and drop the "entry
placeholder replaces the empty state" sentence from its doc.

Delete the whole `// ---- clicks while the entry field is open ----`
section's tests: `a_chevron_click_below_an_open_entry_toggles_that_package`,
`a_cell_click_below_an_open_entry_lands_on_that_row`,
`a_click_on_the_placeholder_only_closes_the_entry`,
`a_double_click_below_an_open_entry_edits_that_row`,
`a_double_click_on_the_placeholder_opens_nothing`,
`a_tree_column_double_click_below_an_open_entry_keeps_that_row`,
`a_later_double_click_at_the_same_spot_edits_the_row_painted_there`,
and `TWO_PACKAGES` if no other test uses it. Keep `THREE_LINES` and add:

```rust
    // ---- clicks while the bar is open ----

    #[gpui::test]
    fn a_click_on_a_row_while_the_bar_is_open_closes_it_and_lands_there(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_seeded(cx, &THREE_LINES);
        h.dispatch(&mut vcx, "add_below", None);
        let at = centre_of(&mut vcx, "pricer-cell-2-2");
        click_at(&mut vcx, at, 1);
        h.draw(&mut vcx);
        assert_eq!(h.mode(&mut vcx), "normal");
        assert_eq!(h.cursor(&vcx).map(|c| c.0), Some(2));
    }

    #[gpui::test]
    fn a_double_click_on_a_row_while_the_bar_is_open_edits_that_row(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_seeded(cx, &THREE_LINES);
        h.dispatch(&mut vcx, "add_below", None);
        let at = centre_of(&mut vcx, "pricer-cell-2-2");
        click_at(&mut vcx, at, 2);
        h.draw(&mut vcx);
        assert_eq!(h.cursor(&vcx).map(|c| c.0), Some(2));
        assert_eq!(h.mode(&mut vcx), "insert", "the cell editor opened");
        assert!(h.tile.read_with(&vcx, |t, _| t.entry.is_none() && t.editor.is_some()));
    }
```

If `click_at(.., 2)` does not produce both presses in this harness, copy
the press pattern from the deleted `a_double_click_below_an_open_entry_edits_that_row`
before deleting it.

- [ ] **Step 8: Run the crate suite**

Run: `cargo test -p geode-pricer`
Expected: all pass. Then `cargo test -p geode-app pricer` — all pass.

- [ ] **Step 9: Mutation entries**

In `scripts/mutation-check.sh`:
- `pricer tile: the entry field is dropped unblurred`: test filter →
  `escape_closes_the_bar_and_the_field_blurs_before_it_drops`.
- `pricer entry: o below a leg lands before it` → rename
  `pricer entry: o on a leg lands before it`, anchor
  `'            leg: row - p,'`, mutation `'            leg: row - p - 1,'`,
  filter `o_lands_after_the_cursor_row`.
- Delete these six entries and their comments:
  `a chevron click reads its row after the entry closes`,
  `a cell click reads its row after the entry closes`,
  `a double-click's second press ignores the first's line`,
  `the closing press's line outlives the next press`,
  `a placeholder press hands nothing on`,
  `a tree-column double-click keeps the slid-up row`.
- `pricer gutter: a cursor move refreshes relative numbers`: anchor
  `'        let stamp = (len, cursor, mode);'`, mutation
  `'        let stamp = (len, None, mode);'`.
- Add:

```zsh
# A refused enter's reason describes the text as it was; an edit must
# clear it, or the bar blames text that is no longer there.
run_mutation "pricer entry bar: an edit keeps a stale error" \
  crates/geode-pricer/src/tile.rs \
  '                && entry.error.take().is_some()' \
  '                && entry.error.clone().is_some()' \
  geode-pricer a_parse_error_shows_under_the_field_keeps_the_text_and_typing_clears_it

# A refused insert must put the place back, or the next enter lands
# past a line that never went in.
run_mutation "pricer entry bar: a refusal keeps the advanced place" \
  crates/geode-pricer/src/tile.rs \
  '                    entry.place = at;
                    entry.error = Some(e.to_string().into());' \
  '                    entry.error = Some(e.to_string().into());' \
  geode-pricer a_package_typed_at_a_leg_place_is_refused_under_the_field

# The label follows the place after each enter.
run_mutation "pricer entry bar: the label stays on the first place" \
  crates/geode-pricer/src/tile.rs \
  '                    entry.label = target_label(&self.sheet, entry.place).into();' \
  '' \
  geode-pricer o_lands_below_the_cursor_row_and_the_label_says_so
```

Run: `zsh scripts/mutation-check.sh "pricer entry"` and
`zsh scripts/mutation-check.sh "pricer gutter"` — every entry caught.
Run: `zsh scripts/mutation-check.sh --anchors-only` — exit 0.

- [ ] **Step 10: Docs**

`docs/current/features.md`, pricer section:
- Replace the key-table row for `o` / `shift+o` with:
  `| \`o\` | Open the entry bar under the header; \`enter\` adds the line below the cursor row (on a leg, the next leg; on a package, its first leg; with no cursor row, at the end) and keeps the bar open for the next; \`up\`/\`down\` walk the sheet's own lines as history; \`escape\` closes it |`
- Add after the tree-column paragraph: "The entry bar sits between the
  header and the column headers. A muted label names where `enter` lands
  (`after <row>`, `into <TEMPLATE>`, `at end`). A parse error or a
  refused insert keeps the text and shows the reason under the field in
  danger text; any edit clears it."
- In the tree-column paragraph delete "; the entry row opens at the
  depth it will land at". In the line-numbers paragraph delete "The entry
  placeholder is blank and does not shift the numbers below it, since no
  motion lands on it."
- Replace the click paragraph "A grid click cancels an editor or entry
  field before acting on the painted row's identity. Removing an entry
  placeholder therefore cannot redirect the click to a neighboring row.
  Clicking the placeholder itself only closes it." with "A grid click
  closes an open editor or the entry bar, then acts on the row it hit."
- Line 484 "Open editors and the entry field paint no field chrome" →
  "Open editors paint no field chrome".

`crates/geode-pricer/README.md`: `entry` row → "Where `o` lands, the
entry bar's label, and the entry history."; `delegate` row drop "entry
row,"; delete the invariant at line ~87 about the placeholder shifting
rows (replace with nothing); line ~179 "A row's own ground (package,
entry)" → "A package row's ground"; line ~186 drop the placeholder
clause; line ~189 stamp "keyed by row count, relative cursor row and
mode".

- [ ] **Step 11: Commit**

```bash
cargo fmt
cargo clippy -p geode-pricer --all-targets -- -D warnings
git add -A crates/geode-pricer scripts/mutation-check.sh docs/current/features.md
git commit -m "feat(pricer): lines are typed in an entry bar, not a placeholder row

o opens a bar under the header; shift+o is gone. Errors show under the
field. With no placeholder row, clicks no longer hand rows across a
close, so that machinery and its six harness entries are retired.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: Column 0 is structural

**Files:**
- Modify: `crates/geode-pricer/src/grid.rs`
- Modify: `crates/geode-pricer/src/delegate.rs`
- Modify: `crates/geode-pricer/src/tile.rs` (find, harness)
- Modify: `scripts/mutation-check.sh`
- Modify: `docs/current/features.md`, `crates/geode-pricer/README.md`

**Interfaces:**
- Produces: `GridRow { .., tag: SharedString, search: SharedString, .. }`
  (replaces `tree`); `Harness::tags(&self, &VisualTestContext) -> Vec<String>`.
  `Harness::tree` keeps its name and returns the search keys.

- [ ] **Step 1: Failing grid tests**

In `grid.rs` tests, rename
`rows_follow_the_expansion_and_carry_depth_ids_and_the_tree_label` to
`rows_follow_the_expansion_and_carry_depth_ids_tags_and_search_keys`,
change `.tree` to `.search` in it, and add after the first `closed`
assertions:

```rust
        assert_eq!(closed.rows[1].tag.as_ref(), "CS", "a package: its template token");
        assert_eq!(closed.rows[0].tag.as_ref(), "", "a line: no tag");
```

and after `open` is built:

```rust
        assert_eq!(open.rows[2].tag.as_ref(), "", "a leg: no tag");
```

Rename `a_custom_package_labels_by_template_underlyings_and_expiries` to
`a_custom_package_is_tagged_by_its_token_and_searched_by_its_legs`:

```rust
        let m = build(&s, &Expansion::default());
        assert_eq!(m.rows[0].tag.as_ref(), "CUSTOM");
        assert_eq!(m.rows[0].search.as_ref(), "CUSTOM SPX Z26");
```

Run: `cargo test -p geode-pricer grid::` — compile error on `tag`/`search`.

- [ ] **Step 2: Implement the model**

In `grid.rs` replace the `tree` field with:

```rust
    /// Column 0's painted tag: a package's template token (`CS`,
    /// `CUSTOM`), empty on a line or leg (entry-bar spec §2).
    pub tag: SharedString,
    /// What find matches: the row's shorthand, never painted, so `/`
    /// finds a row by text no visible column shows (entry-bar spec §3).
    pub search: SharedString,
```

Rename `package_label` to `package_search` with doc "A package's search
key: its template form while the legs still match the table (the grammar
round-trips it), else its template token with its legs' distinct
underlyings and expiries." In `build`:

```rust
            let (tag, search) = match sheet.kind(r) {
                RowKind::Package { template } => (
                    SharedString::new_static(template.token()),
                    package_search(sheet, r),
                ),
                _ => (SharedString::default(), sheet.shorthand(r)),
            };
```

and in the `GridRow` literal `tag, search: search.into(),`. Update the
module doc: "Column 0 carries structure only: the row's tag; the
shorthand is kept as a search key."

In `tile.rs`: `row_labels` reads `r.search`; the harness `tree` reads
`r.search`, doc "Every painted row's search key (its shorthand)"; add

```rust
        /// Every painted row's column-0 tag.
        pub fn tags(&self, vcx: &VisualTestContext) -> Vec<String> {
            self.tile.read_with(vcx, |t, _| {
                t.model.rows.iter().map(|r| r.tag.to_string()).collect()
            })
        }
```

In `delegate.rs` `render_cell`'s tree branch: `.child(row.tag.clone())`
and comment "then the row's tag (a package's template token)".

- [ ] **Step 3: Width**

Failing test first: add to `delegate.rs` `mod width_tests`:

```rust
    /// Column 0 holds a leg's indent, the chevron slot and the widest tag
    /// at the largest font size. Indent and slot are on the rem scale;
    /// the column is fixed pixels.
    #[test]
    fn the_tree_column_fits_the_widest_tag() {
        use super::{CHEVRON_SLOT, INDENT, TREE_WIDTH};
        use crate::core::template::Template;
        let rem = FontSize::Large.rem_px();
        let advance = rem * TABLE_TEXT_REM * MONO_ADVANCE_EM;
        let pad = Size::XSmall.table_cell_padding();
        let padding = f32::from(pad.left) + f32::from(pad.right);
        let widest = Template::ALL
            .iter()
            .map(|t| t.token().chars().count())
            .max()
            .unwrap();
        let need = (INDENT + CHEVRON_SLOT) * rem / geode_shell::shell::scale::DESIGN_REM
            + widest as f32 * advance
            + padding;
        assert!(need <= TREE_WIDTH, "needs {need:.1}px in {TREE_WIDTH}px");
        assert!(TREE_WIDTH - need < 16.0, "{TREE_WIDTH}px wastes {:.1}px", TREE_WIDTH - need);
    }
```

If `Template` has no `ALL` slice, use a literal array of the eight
variants (`Custom, CS, PS, STRD, STRG, RR, FLY, CAL`). Run
`cargo test -p geode-pricer the_tree_column_fits_the_widest_tag`; it
fails on the waste bound at `260.0`. Set `TREE_WIDTH` to the smallest
multiple of 4 that passes (print `need` from the failure) and update its
doc: "The tree column: a leg's indent, the chevron slot and the widest
template token at the largest font size (checked below), in pixels like
every width here; not resizable." Change the column's `name` from
`"line"` to `""` (the tag needs no heading).

- [ ] **Step 4: Find by hidden text**

Add to `tile.rs` after `find_jumps_from_its_origin_repeats_with_n_and_escape_returns`:

```rust
    /// Find matches the shorthand even though column 0 no longer paints
    /// it: a package's strikes show in no package-row cell.
    #[gpui::test]
    fn find_matches_shorthand_that_no_column_shows(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        assert_eq!(h.tags(&vcx), ["", "CS", ""]);
        vcx.update(|window, cx| {
            h.content
                .find(FindEvent::Changed("4800/5200".into()), window, cx)
        });
        assert_eq!(h.cursor(&vcx).map(|c| c.0), Some(1));
    }
```

- [ ] **Step 5: Run**

Run: `cargo test -p geode-pricer`
Expected: all pass.

- [ ] **Step 6: Mutation entries**

Add to `scripts/mutation-check.sh` near the other `pricer grid`/`pricer
gutter` entries:

```zsh
# Column 0 paints only a package's template token.
run_mutation "pricer grid: a package's tag is its search text" \
  crates/geode-pricer/src/grid.rs \
  '                    SharedString::new_static(template.token()),' \
  '                    package_search(sheet, r).into(),' \
  geode-pricer rows_follow_the_expansion_and_carry_depth_ids_tags_and_search_keys

# Find reads the unpainted search key, not the painted tag.
run_mutation "pricer tile: find reads the painted tag" \
  crates/geode-pricer/src/tile.rs \
  '        self.model.rows.iter().map(|r| r.search.to_string()).collect()' \
  '        self.model.rows.iter().map(|r| r.tag.to_string()).collect()' \
  geode-pricer find_matches_shorthand_that_no_column_shows
```

The harness `tree` helper has the same line with different indentation
(12 spaces inside `read_with`); if `--anchors-only` reports AMBIG, anchor
on the two lines `    fn row_labels(&self) -> Vec<String> {` + the
body line instead.

Run: `zsh scripts/mutation-check.sh "pricer grid: a package"` and
`zsh scripts/mutation-check.sh "find reads the painted tag"` — caught.
Run: `zsh scripts/mutation-check.sh --anchors-only` — exit 0.

- [ ] **Step 7: Docs**

`docs/current/features.md` tree-column paragraph: replace "A long tree
label or text cell ends in `…`" with "Column 0 carries structure only:
the depth indent, the chevron slot and a package's template token (`CS`,
`CUSTOM`); a line or leg has no tag. Find (`/`, `n`, `N`) still matches
each row's full shorthand, which no column paints. A long text cell ends
in `…`". Keep the rest.

`crates/geode-pricer/README.md`: `delegate` row → "cells, the tree
column (indent, chevron, template tag), editor, expiry date field";
replace the line "Shorthand rendering uses a template only while the legs
still match it." with "Shorthand rendering uses a template only while
the legs still match it; the grid keeps it as the row's find key and
paints only a package's template token."

- [ ] **Step 8: Commit**

```bash
cargo fmt
cargo clippy -p geode-pricer --all-targets -- -D warnings
git add -A crates/geode-pricer scripts/mutation-check.sh docs/current/features.md
git commit -m "feat(pricer): column 0 shows structure and a package's template only

Find keeps matching each row's full shorthand as an unpainted key.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: Branch verification

- [ ] **Step 1: Workspace gates**

Run each; each must succeed:

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo check -p geode-shell --features test-support --all-targets
zsh scripts/mutation-check.sh --anchors-only
zsh scripts/mutation-check.sh --changed
```

Skip local `cargo bench --no-run` (CI runs it). `--changed` runs detached
if it takes more than a few minutes; every entry must report caught.

- [ ] **Step 2: Display check list**

Record for the user (not automatable headlessly): the bar's height and
border against the header, the muted label's truncation on a long
shorthand, the danger line under the field, and column 0's width with
`line_numbers` on and off at all three font sizes.
